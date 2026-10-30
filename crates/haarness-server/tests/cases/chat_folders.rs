//! A chat with a folder of its own (`CreateThreadRequest::directory`): the
//! server makes the folder inside the project, and the chat's tools, prompt,
//! permissions and undo work there; children and forks work there too. An
//! ordinary chat keeps working in the project folder.
use super::*;
use base64::Engine as _;
use haarness_protocol::{ItemData, PermissionReply, PermissionReplyDecision, ThreadId};
use serde_json::json;
use std::path::PathBuf;

struct Fixture {
    root: PathBuf,
    /// The project folder (the chat home in the app), canonical.
    workspace: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("haarness-chat-folders-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(root.join("home")).unwrap();
        let root = root.canonicalize().unwrap();
        Self {
            workspace: root.join("home"),
            root,
        }
    }

    /// A runtime over the project folder whose chats' own folders are
    /// checkpointed (as the CLI composes it), and the requests its model got.
    fn runtime(&self) -> (AgentRuntime, Arc<std::sync::Mutex<Vec<ProviderRequest>>>) {
        self.runtime_with(|database| haarness_store::SqliteSnapshotFolders::open(database))
    }

    fn runtime_with(
        &self,
        folders: fn(&std::path::Path) -> haarness_store::SqliteSnapshotFolders,
    ) -> (AgentRuntime, Arc<std::sync::Mutex<Vec<ProviderRequest>>>) {
        let database = self.root.join("db.sqlite");
        let provider = Capturing::default();
        let requests = provider.requests.clone();
        let runtime = AgentRuntime::with_config(
            Arc::new(SqliteStore::open(&database).unwrap()),
            Arc::new(StaticProviderRegistry::new(vec![
                Arc::new(provider) as Arc<dyn ModelProvider>
            ])),
            Arc::new(NativeToolRegistry::new(self.workspace.clone())),
            EventBus::new(256),
            self.workspace.clone(),
            String::new(),
            RuntimeConfig {
                approval_mode: haarness_protocol::ApprovalMode::Review,
                ..RuntimeConfig::default()
            },
        )
        .with_folder_snapshots(Arc::new(folders(&database)));
        (runtime, requests)
    }

    /// The same runtime, run as the chat home (whose chats' folders the CLI
    /// checkpoints standing alone).
    fn chat_home(&self) -> (AgentRuntime, Arc<std::sync::Mutex<Vec<ProviderRequest>>>) {
        let (runtime, requests) = self.runtime_with(|database| {
            haarness_store::SqliteSnapshotFolders::open_in_chat_home(database)
        });
        (runtime.as_chat_home(), requests)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

async fn chat(runtime: &AgentRuntime, directory: Option<&str>) -> CoreResult<Thread> {
    runtime
        .create_thread(CreateThreadRequest {
            title: Some("Fixture".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            directory: directory.map(str::to_owned),
            ..Default::default()
        })
        .await
}

/// Run `prompt` (an echo directive) as one turn of `thread` to its end.
async fn turn(runtime: &AgentRuntime, thread: ThreadId, prompt: &str) {
    let turn = runtime
        .start_turn(thread, turn_request(prompt))
        .await
        .unwrap();
    assert_eq!(
        wait_done(runtime, &turn).await.status,
        TurnStatus::Completed
    );
}

/// The model's `bash` call printing its working folder.
fn pwd() -> String {
    format!("!tool bash {}", json!({ "command": "pwd -P" }))
}

/// The output of the last `name` result of `thread`.
async fn last_result(runtime: &AgentRuntime, thread: ThreadId, name: &str) -> String {
    runtime
        .items(thread, None)
        .await
        .unwrap()
        .into_iter()
        .rev()
        .find_map(|item| match item.data {
            ItemData::ToolResult {
                name: found,
                output,
                ..
            } if found == name => Some(output),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no {name} result"))
}

fn folder(thread: &Thread) -> PathBuf {
    PathBuf::from(thread.directory.as_deref().expect("the chat has a folder"))
}

/// Hooks that write one line per run to `log`: where they ran, the project
/// folder they got in their environment and as a placeholder, then their
/// payload.
#[cfg(unix)]
fn recording_hooks(
    project: &std::path::Path,
    log: &std::path::Path,
) -> haarness_server::ConfiguredHooks {
    let record = format!(
        r#"printf '%s|%s|%s|' "$(pwd -P)" "$CLAUDE_PROJECT_DIR" "${{CLAUDE_PROJECT_DIR}}" >> '{log}'; cat >> '{log}'; echo >> '{log}'"#,
        log = log.display()
    );
    let group = json!([{"hooks": [{"command": record}]}]);
    haarness_server::ConfiguredHooks::new(
        haarness_server::ConfiguredHooks::parse_block(&json!({
            "SessionStart": group, "UserPromptSubmit": group, "PreToolUse": group,
            "PostToolUse": group, "Stop": group, "SessionEnd": group,
        })),
        project.to_path_buf(),
    )
}

/// Checks the runs `log` holds for `thread`: their events in order, each
/// run's folder (also its `cwd`) and the project folder it was told, and the
/// agent named where one acts.
#[cfg(unix)]
fn assert_hook_runs(
    log: &std::path::Path,
    thread: &Thread,
    expected: &[(&str, &PathBuf, &PathBuf)],
) {
    let written = std::fs::read_to_string(log).unwrap();
    let runs: Vec<(Vec<&str>, Value)> = written
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.splitn(4, '|').collect();
            let payload: Value = serde_json::from_str(parts[3]).unwrap();
            (payload["session_id"] == json!(thread.id)).then(|| (parts[..3].to_vec(), payload))
        })
        .collect();
    let events: Vec<&str> = runs
        .iter()
        .map(|(_, payload)| payload["hook_event_name"].as_str().unwrap())
        .collect();
    let expected_events: Vec<&str> = expected.iter().map(|(event, _, _)| *event).collect();
    assert_eq!(events, expected_events, "{}", thread.title);
    for ((folders, payload), (_, cwd, told)) in runs.iter().zip(expected) {
        let (cwd, told) = (cwd.display().to_string(), told.display().to_string());
        assert_eq!(
            folders,
            &[cwd.as_str(), told.as_str(), told.as_str()],
            "{payload}"
        );
        assert_eq!(payload["cwd"], json!(cwd), "{payload}");
        let named = matches!(
            payload["hook_event_name"].as_str(),
            Some("SessionStart" | "UserPromptSubmit" | "PreToolUse" | "PostToolUse")
        );
        assert_eq!(payload.get("agent").is_some(), named, "{payload}");
        if named {
            assert_eq!(payload["agent"], json!(thread.agent), "{payload}");
        }
    }
}

/// The update that archives a chat.
#[cfg(unix)]
fn archived() -> haarness_protocol::UpdateThreadRequest {
    haarness_protocol::UpdateThreadRequest {
        title: None,
        archived: Some(true),
        agent: None,
        provider: None,
        model: None,
        ultracode: None,
    }
}

/// On a project's server, a chat with a folder of its own runs its hooks
/// there, from its start to its tools, a person's own command, the end of
/// its turns and its archiving, with the folder as their `cwd`; they are told
/// the server's project folder, where its hook scripts are, and the agent
/// where one acts. An ordinary chat's hooks run in the project folder, as do
/// those of a chat whose folder is gone, which nothing makes again for them.
#[cfg(unix)]
#[tokio::test]
async fn a_chats_hooks_run_in_its_own_folder() {
    let fixture = Fixture::new();
    let log = fixture.root.join("hooks.log");
    let (runtime, _) = fixture.runtime();
    let runtime = runtime.with_hooks(Arc::new(recording_hooks(&fixture.workspace, &log)));
    let own = chat(&runtime, Some("2026-10-28/notes")).await.unwrap();
    let plain = chat(&runtime, None).await.unwrap();
    let gone = chat(&runtime, Some("2026-10-28/gone")).await.unwrap();
    for thread in [&own, &plain] {
        turn(&runtime, thread.id, &pwd()).await;
    }
    let ran = last_result(&runtime, own.id, "bash").await;
    assert!(ran.contains(&folder(&own).display().to_string()), "{ran}");
    runtime.run_shell(own.id, "pwd -P".into()).await.unwrap();
    std::fs::remove_dir(folder(&gone)).unwrap();
    for thread in [&own, &gone] {
        runtime.update_thread(thread.id, archived()).await.unwrap();
    }
    let (own_folder, project) = (folder(&own), fixture.workspace.clone());
    let in_own = |event| (event, &own_folder, &project);
    assert_hook_runs(
        &log,
        &own,
        &[
            in_own("SessionStart"),
            in_own("UserPromptSubmit"),
            in_own("PreToolUse"),
            in_own("PostToolUse"),
            in_own("Stop"),
            in_own("PreToolUse"),
            in_own("PostToolUse"),
            in_own("SessionEnd"),
        ],
    );
    let in_project = |event| (event, &project, &project);
    assert_hook_runs(
        &log,
        &plain,
        &[
            in_project("SessionStart"),
            in_project("UserPromptSubmit"),
            in_project("PreToolUse"),
            in_project("PostToolUse"),
            in_project("Stop"),
        ],
    );
    let gone_folder = folder(&gone);
    assert_hook_runs(
        &log,
        &gone,
        &[
            ("SessionStart", &gone_folder, &project),
            in_project("SessionEnd"),
        ],
    );
}

/// On the chat home a chat's folder is the only project it has: its hooks
/// run there and are told it as the project folder too.
#[cfg(unix)]
#[tokio::test]
async fn a_chat_home_chats_hooks_take_its_folder_as_their_project() {
    let fixture = Fixture::new();
    let log = fixture.root.join("hooks.log");
    let (runtime, _) = fixture.chat_home();
    let runtime = runtime.with_hooks(Arc::new(recording_hooks(&fixture.workspace, &log)));
    let thread = chat(&runtime, None).await.unwrap();
    turn(&runtime, thread.id, &pwd()).await;
    runtime.run_shell(thread.id, "pwd -P".into()).await.unwrap();
    runtime.update_thread(thread.id, archived()).await.unwrap();
    let own = folder(&thread);
    assert!(own.starts_with(&fixture.workspace) && own != fixture.workspace);
    let mine = |event| (event, &own, &own);
    assert_hook_runs(
        &log,
        &thread,
        &[
            mine("SessionStart"),
            mine("UserPromptSubmit"),
            mine("PreToolUse"),
            mine("PostToolUse"),
            mine("Stop"),
            mine("PreToolUse"),
            mine("PostToolUse"),
            mine("SessionEnd"),
        ],
    );
}

/// A chat's folder deleted while a turn runs (here by the turn's own
/// `UserPromptSubmit` hook, after the turn made it again) cannot hold the
/// turn's later hooks: they run in the project folder, so a `PreToolUse`
/// guard still refuses the write it guards against.
#[cfg(unix)]
#[tokio::test]
async fn a_guard_still_refuses_after_the_chats_folder_goes_mid_turn() {
    let fixture = Fixture::new();
    let (doomed, log) = (
        fixture.workspace.join("2026-10-28/doomed"),
        fixture.root.join("guard.log"),
    );
    let remove = format!("rmdir '{}'", doomed.display());
    let guard = format!(
        "pwd -P >> '{log}'; echo 'guard: no writes' >&2; exit 2",
        log = log.display()
    );
    let hooks = haarness_server::ConfiguredHooks::new(
        haarness_server::ConfiguredHooks::parse_block(&json!({
            "UserPromptSubmit": [{"hooks": [{"command": remove}]}],
            "PreToolUse": [{"matcher": "write", "hooks": [{"command": guard}]}],
        })),
        fixture.workspace.clone(),
    );
    let (runtime, _) = fixture.runtime();
    let runtime = runtime.with_hooks(Arc::new(hooks));
    let thread = chat(&runtime, Some("2026-10-28/doomed")).await.unwrap();
    assert_eq!(folder(&thread), doomed);
    let write = json!({"path": "notes.md", "content": "x"});
    turn(&runtime, thread.id, &format!("!tool write {write}")).await;
    assert!(!doomed.exists(), "the folder went during the turn");
    let refused = last_result(&runtime, thread.id, "write").await;
    assert!(refused.contains("guard: no writes"), "{refused}");
    let ran_in = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        ran_in.trim_end(),
        fixture.workspace.display().to_string(),
        "the guard ran in the project folder"
    );
    assert!(!fixture.workspace.join("notes.md").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn a_chat_folder_is_made_inside_numbered_when_taken_and_refused_outside() {
    let fixture = Fixture::new();
    let (runtime, _) = fixture.runtime();
    let first = chat(&runtime, Some("2026-10-24/hello")).await.unwrap();
    assert_eq!(folder(&first), fixture.workspace.join("2026-10-24/hello"));
    assert!(folder(&first).is_dir());
    // It is stored, listed and read back.
    assert_eq!(
        runtime.get_thread(first.id).await.unwrap().directory,
        first.directory
    );
    assert!(
        runtime
            .list_threads(false)
            .await
            .unwrap()
            .iter()
            .any(|thread| thread.id == first.id && thread.directory == first.directory)
    );
    // Taken by a chat, the next one gets the next number.
    let second = chat(&runtime, Some("2026-10-24/hello")).await.unwrap();
    assert_eq!(
        folder(&second),
        fixture.workspace.join("2026-10-24/hello-2")
    );
    let third = chat(&runtime, Some("2026-10-24/hello")).await.unwrap();
    assert_eq!(folder(&third), fixture.workspace.join("2026-10-24/hello-3"));
    // A folder that exists but no chat uses is taken as it is.
    std::fs::create_dir_all(fixture.workspace.join("2026-10-24/kept")).unwrap();
    std::fs::write(fixture.workspace.join("2026-10-24/kept/notes.md"), "mine").unwrap();
    let kept = chat(&runtime, Some("2026-10-24/kept")).await.unwrap();
    assert_eq!(folder(&kept), fixture.workspace.join("2026-10-24/kept"));
    // A chat without one has none.
    assert_eq!(chat(&runtime, None).await.unwrap().directory, None);
    // Two asking for one folder at once get two.
    let (left, right) = tokio::join!(
        chat(&runtime, Some("2026-10-24/race")),
        chat(&runtime, Some("2026-10-24/race"))
    );
    let mut raced = [folder(&left.unwrap()), folder(&right.unwrap())];
    raced.sort();
    assert_eq!(
        raced,
        [
            fixture.workspace.join("2026-10-24/race"),
            fixture.workspace.join("2026-10-24/race-2")
        ]
    );

    // Named outside the project, absolute, empty or odd: refused, plainly.
    // A file where a folder would be is passed over, as a taken name is.
    std::fs::write(fixture.workspace.join("2026-10-24/taken"), "a file").unwrap();
    let past_file = chat(&runtime, Some("2026-10-24/taken")).await.unwrap();
    assert_eq!(
        folder(&past_file),
        fixture.workspace.join("2026-10-24/taken-2")
    );
    // A chat's absolute folder is shared as it is (a schedule's runs).
    let shared = chat(&runtime, first.directory.as_deref()).await.unwrap();
    assert_eq!(shared.directory, first.directory);

    // Named outside the project, absolute, empty, at or below another
    // chat's folder, through a link, or not `<date>/<name>`: refused, plainly.
    std::fs::create_dir_all(fixture.root.join("outside")).unwrap();
    std::os::unix::fs::symlink(
        fixture.root.join("outside"),
        fixture.workspace.join("2026-10-22"),
    )
    .unwrap();
    std::os::unix::fs::symlink(".", fixture.workspace.join("2026-10-21")).unwrap();
    std::os::unix::fs::symlink(folder(&first), fixture.workspace.join("2026-10-24/alias")).unwrap();
    for refused in [
        "../escape",
        "/tmp/x",
        fixture.root.join("outside").to_str().unwrap(),
        fixture.workspace.join("2026-10-24").to_str().unwrap(),
        "",
        "2026-10-24",
        "2026-10-24/hello/inner",
        "2026-10-24//b",
        "2026-10-24/./b",
        "day/chat",
        "~/x",
        "2026-10-22/chat",
        "2026-10-21/chat",
    ] {
        let error = chat(&runtime, Some(refused)).await.unwrap_err();
        assert!(matches!(error, CoreError::Invalid(_)), "{refused}: {error}");
    }
    assert!(!fixture.root.join("escape").exists());
    assert!(
        !fixture.root.join("outside/chat").exists(),
        "nothing made through the link"
    );
    assert!(!fixture.workspace.join("chat").exists());
    // A link where a chat's folder would be is passed over like a file.
    let past_link = chat(&runtime, Some("2026-10-24/alias")).await.unwrap();
    assert_eq!(
        folder(&past_link),
        fixture.workspace.join("2026-10-24/alias-2")
    );
}

#[tokio::test]
async fn the_api_refuses_a_folder_outside_the_project_and_says_it_takes_one() {
    let fixture = Fixture::new();
    let (runtime, _) = fixture.runtime();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let handle = serve(
        listener,
        "fixture-token",
        ServerState {
            browser: None,
            management: None,
            runtime: Arc::new(runtime),
            mcp: Arc::new(NoMcp),
            skills: Arc::new(NoSkills),
            plugins: None,
            workflows: None,
            workspaces: None,
            workspace_id: None,
            workspace: fixture.workspace.clone(),
            project_trusted: false,
            language: None,
        },
    )
    .await
    .unwrap();
    let base = format!("http://{}", handle.address);
    let client = reqwest::Client::new();
    let info: ServerInfo = client
        .get(format!("{base}/v1/info"))
        .bearer_auth("fixture-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        info.capabilities
            .iter()
            .any(|capability| capability == haarness_protocol::THREAD_DIRECTORY_CAPABILITY)
    );
    let refused = client
        .post(format!("{base}/v1/threads"))
        .bearer_auth("fixture-token")
        .json(&json!({"provider": "echo", "model": "echo-1", "directory": "../elsewhere"}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = refused.json().await.unwrap();
    assert_eq!(body["code"], "invalid_request", "{body}");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|message| message.contains("../elsewhere")),
        "{body}"
    );
    let created: Thread = client
        .post(format!("{base}/v1/threads"))
        .bearer_auth("fixture-token")
        .json(&json!({"provider": "echo", "model": "echo-1", "directory": "2026-10-24/chat"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(folder(&created), fixture.workspace.join("2026-10-24/chat"));
    // The wire form names it `directory`.
    let listed: Value = client
        .get(format!("{base}/v1/threads"))
        .bearer_auth("fixture-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .any(|thread| thread["directory"] == json!(created.directory)),
        "{listed}"
    );
    // The chat's own workflows are listed for it (as workflows and as
    // commands), and for nobody else.
    let workflows = folder(&created).join(".haarness/workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    std::fs::write(
        workflows.join("mine.js"),
        "export const meta = { name: 'mine', description: 'Mine' }\nreturn 1\n",
    )
    .unwrap();
    let get = |path: String| {
        let client = client.clone();
        async move {
            client
                .get(path)
                .bearer_auth("fixture-token")
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    };
    let names = |listed: &Value| {
        listed["workflows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|workflow| workflow["invocation"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let for_chat = get(format!("{base}/v1/workflows?threadId={}", created.id)).await;
    assert_eq!(names(&for_chat), ["mine"], "{for_chat}");
    assert!(names(&get(format!("{base}/v1/workflows")).await).is_empty());
    let commands = get(format!("{base}/v1/commands?threadId={}", created.id)).await;
    assert!(
        commands
            .as_array()
            .unwrap()
            .iter()
            .any(|command| command["name"] == "mine"),
        "{commands}"
    );
    let plain = get(format!("{base}/v1/commands")).await;
    assert!(
        !plain
            .as_array()
            .unwrap()
            .iter()
            .any(|command| command["name"] == "mine")
    );

    // A published chat lists for itself only, never for another chat.
    let other = client
        .post(format!("{base}/v1/threads"))
        .bearer_auth("fixture-token")
        .json(&json!({"provider": "echo", "model": "echo-1", "directory": "2026-10-24/other"}))
        .send()
        .await
        .unwrap()
        .json::<Thread>()
        .await
        .unwrap();
    std::fs::create_dir_all(folder(&other).join(".haarness/workflows")).unwrap();
    std::fs::write(
        folder(&other).join(".haarness/workflows/theirs.js"),
        "export const meta = { name: 'theirs' }\nreturn 1\n",
    )
    .unwrap();
    let scoped = |path: String| {
        client
            .get(path)
            .bearer_auth("fixture-token")
            .header("x-haarness-remote-thread", created.id.to_string())
            .send()
    };
    assert_eq!(
        scoped(format!("{base}/v1/commands?threadId={}", created.id))
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::OK
    );
    for path in [
        format!("{base}/v1/commands?threadId={}", other.id),
        format!("{base}/v1/tools?threadId={}", other.id),
        format!("{base}/v1/commands?threadId=not-an-id"),
    ] {
        assert_eq!(
            scoped(path.clone()).await.unwrap().status(),
            reqwest::StatusCode::FORBIDDEN,
            "{path}"
        );
    }

    // A chat folder a link now stands in for is not followed for a list.
    std::fs::remove_dir_all(folder(&created)).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(folder(&other), folder(&created)).unwrap();
        for path in [
            format!("{base}/v1/workflows?threadId={}", created.id),
            format!("{base}/v1/commands?threadId={}", created.id),
        ] {
            let refused = client
                .get(path.clone())
                .bearer_auth("fixture-token")
                .send()
                .await
                .unwrap();
            assert_eq!(refused.status(), reqwest::StatusCode::BAD_REQUEST, "{path}");
            assert!(!refused.text().await.unwrap().contains("theirs"), "{path}");
        }
    }
}

#[tokio::test]
async fn a_chat_works_in_its_folder_and_a_sibling_s_folder_is_outside() {
    let fixture = Fixture::new();
    let (runtime, requests) = fixture.runtime();
    let a = chat(&runtime, Some("2026-10-24/a")).await.unwrap();
    let b = chat(&runtime, Some("2026-10-24/b")).await.unwrap();

    // The model's shell, a person's `!` and a workflow's shell run there.
    for thread in [&a, &b] {
        turn(&runtime, thread.id, &pwd()).await;
        assert_eq!(
            last_result(&runtime, thread.id, "bash").await,
            format!("exit: 0\n{}\n", folder(thread).display())
        );
    }
    let person = runtime.run_shell(a.id, "pwd -P".into()).await.unwrap();
    assert!(
        matches!(&person.data, ItemData::ToolResult { output, .. }
            if *output == format!("exit: 0\n{}\n", folder(&a).display())),
        "{:?}",
        person.data
    );
    let workflow = runtime
        .run_workflow_tool(
            b.id,
            "bash",
            json!({"command": "pwd -P"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        workflow.output.metadata["stdout"],
        format!("{}\n", folder(&b).display())
    );

    // The prompt names it as the working folder.
    let prompt = requests
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|request| request.system.contains("Working directory:"))
        .map(|request| request.system.clone())
        .unwrap();
    assert!(
        prompt.contains(&format!("Working directory: {}", folder(&b).display())),
        "{prompt}"
    );
    assert!(prompt.contains("Is directory a git repo: no"));

    // Files go there; a listing and a search see only it.
    turn(
        &runtime,
        a.id,
        r#"!tool write {"path":"note.txt","content":"from a"}"#,
    )
    .await;
    assert_eq!(
        std::fs::read_to_string(folder(&a).join("note.txt")).unwrap(),
        "from a"
    );
    assert!(!fixture.workspace.join("note.txt").exists());
    // A preview is named from the project folder, as the file panel opens it.
    std::fs::write(folder(&a).join("page.html"), "<h1>A</h1>").unwrap();
    let preview = runtime
        .run_direct_tool(
            a.id,
            "preview",
            json!({"filePath": "page.html"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(!preview.is_error, "{}", preview.output.text);
    assert_eq!(
        preview.output.metadata["preview"]["path"],
        "2026-10-24/a/page.html"
    );
    turn(&runtime, b.id, r#"!tool list {"path":"."}"#).await;
    assert_eq!(last_result(&runtime, b.id, "list").await, "");
    turn(&runtime, a.id, r#"!tool glob {"pattern":"*.txt"}"#).await;
    assert!(
        last_result(&runtime, a.id, "glob")
            .await
            .contains("note.txt")
    );

    // Writing into the sibling's folder is outside the chat's project: it
    // asks, and a no writes nothing.
    let sibling = folder(&b).join("intrusion.txt");
    let started = runtime
        .start_turn(
            a.id,
            turn_request(&format!(
                "!tool write {}",
                json!({"path": "../b/intrusion.txt", "content": "no"})
            )),
        )
        .await
        .unwrap();
    let permission = wait_permission(&runtime, a.id).await;
    let asked = runtime
        .items(a.id, None)
        .await
        .unwrap()
        .into_iter()
        .find_map(|item| match item.data {
            ItemData::PermissionRequest {
                permission_id,
                permission: kind,
                patterns,
                ..
            } if permission_id == permission => Some((kind, patterns)),
            _ => None,
        })
        .unwrap();
    assert_eq!(asked.0, "external_directory");
    assert!(
        asked
            .1
            .iter()
            .any(|pattern| pattern.contains(&folder(&b).display().to_string())),
        "{:?}",
        asked.1
    );
    runtime
        .reply_permission(
            &permission,
            PermissionReply {
                decision: PermissionReplyDecision::Reject,
                feedback: None,
            },
        )
        .await
        .unwrap();
    wait_done(&runtime, &started).await;
    assert!(!sibling.exists());
}

#[tokio::test]
async fn a_relative_attachment_is_read_from_the_chat_folder() {
    let fixture = Fixture::new();
    let (runtime, _) = fixture.runtime();
    let thread = chat(&runtime, Some("2026-10-24/attach")).await.unwrap();
    std::fs::write(folder(&thread).join("notes.txt"), "the chat's notes").unwrap();
    std::fs::write(fixture.workspace.join("notes.txt"), "the home's notes").unwrap();
    let mut request = turn_request("read this");
    request.attachments = vec!["notes.txt".into()];
    let started = runtime.start_turn(thread.id, request).await.unwrap();
    wait_done(&runtime, &started).await;
    let attached = runtime
        .items(thread.id, None)
        .await
        .unwrap()
        .into_iter()
        .find_map(|item| match item.data {
            ItemData::UserMessage { attachments, .. } if !attachments.is_empty() => {
                Some(attachments)
            }
            _ => None,
        })
        .unwrap();
    let encoded = attached[0].rsplit_once(',').unwrap().1;
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap(),
        b"the chat's notes"
    );
}

#[tokio::test]
async fn undo_in_a_chat_folder_restores_that_folder_alone() {
    let fixture = Fixture::new();
    let (runtime, _) = fixture.runtime();
    let a = chat(&runtime, Some("2026-10-24/a")).await.unwrap();
    let b = chat(&runtime, Some("2026-10-24/b")).await.unwrap();
    turn(
        &runtime,
        a.id,
        r#"!tool write {"path":"a.txt","content":"one"}"#,
    )
    .await;
    turn(
        &runtime,
        b.id,
        r#"!tool write {"path":"b.txt","content":"two"}"#,
    )
    .await;
    // A file of the project folder itself is no chat's.
    std::fs::write(fixture.workspace.join("loose.txt"), "loose").unwrap();

    let review = runtime.diff_last_turn(a.id).await.unwrap();
    assert_eq!(
        review
            .iter()
            .map(|file| file.file.as_str())
            .collect::<Vec<_>>(),
        ["a.txt"]
    );
    let reverted = runtime.revert(a.id, None).await.unwrap();
    assert!(reverted.revert.is_some());
    assert!(
        !folder(&a).join("a.txt").exists(),
        "the chat's write is undone"
    );
    assert_eq!(
        std::fs::read_to_string(folder(&b).join("b.txt")).unwrap(),
        "two",
        "another chat's file stays"
    );
    assert!(fixture.workspace.join("loose.txt").exists());
    runtime.unrevert(a.id).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(folder(&a).join("a.txt")).unwrap(),
        "one"
    );
}

#[tokio::test]
async fn children_and_forks_work_in_their_parent_s_folder() {
    let fixture = Fixture::new();
    let (runtime, _) = fixture.runtime();
    let parent = chat(&runtime, Some("2026-10-24/parent")).await.unwrap();
    turn(&runtime, parent.id, &tasks_prompt(1, "pwd -P")).await;
    let child = runtime
        .list_threads(false)
        .await
        .unwrap()
        .into_iter()
        .find(|thread| thread.parent_id == Some(parent.id))
        .expect("the task's child");
    assert_eq!(child.directory, parent.directory);
    assert_eq!(
        last_result(&runtime, child.id, "bash").await,
        format!("exit: 0\n{}\n", folder(&parent).display())
    );
    let fork = runtime.fork_thread(parent.id, None, None).await.unwrap();
    assert_eq!(fork.directory, parent.directory);
    turn(&runtime, fork.id, &pwd()).await;
    assert_eq!(
        last_result(&runtime, fork.id, "bash").await,
        format!("exit: 0\n{}\n", folder(&parent).display())
    );
    // A chat a workflow starts under it (a parent and no folder named).
    let workflow_chat = runtime
        .create_thread(CreateThreadRequest {
            parent_id: Some(parent.id),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(workflow_chat.directory, parent.directory);
}

#[tokio::test]
async fn a_plan_is_written_in_the_chat_folder_and_nothing_else_is() {
    let fixture = Fixture::new();
    let (runtime, _) = fixture.runtime();
    let thread = runtime
        .create_thread(CreateThreadRequest {
            title: Some("Fixture".into()),
            agent: Some("plan".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            directory: Some("2026-10-24/plan".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    turn(
        &runtime,
        thread.id,
        r#"!tool write {"path":".haarness/plans/next.md","content":"1. Look"}"#,
    )
    .await;
    assert_eq!(
        std::fs::read_to_string(folder(&thread).join(".haarness/plans/next.md")).unwrap(),
        "1. Look"
    );
    assert!(!fixture.workspace.join(".haarness").exists());
    turn(
        &runtime,
        thread.id,
        r#"!tool write {"path":"code.rs","content":"fn main() {}"}"#,
    )
    .await;
    assert!(
        !folder(&thread).join("code.rs").exists(),
        "plan writes only plans"
    );
}

#[tokio::test]
async fn a_deleted_chat_folder_is_made_again_for_its_next_turn() {
    let fixture = Fixture::new();
    let (runtime, _) = fixture.runtime();
    let thread = chat(&runtime, Some("2026-10-24/gone")).await.unwrap();
    std::fs::remove_dir_all(folder(&thread)).unwrap();
    turn(&runtime, thread.id, &pwd()).await;
    assert!(folder(&thread).is_dir());
    assert_eq!(
        last_result(&runtime, thread.id, "bash").await,
        format!("exit: 0\n{}\n", folder(&thread).display())
    );
}

#[tokio::test]
async fn an_ordinary_chat_works_in_the_project_folder_as_before() {
    let fixture = Fixture::new();
    let (runtime, requests) = fixture.runtime();
    let thread = chat(&runtime, None).await.unwrap();
    assert_eq!(thread.directory, None);
    turn(&runtime, thread.id, &pwd()).await;
    assert_eq!(
        last_result(&runtime, thread.id, "bash").await,
        format!("exit: 0\n{}\n", fixture.workspace.display())
    );
    turn(
        &runtime,
        thread.id,
        r#"!tool write {"path":"plain.txt","content":"plain"}"#,
    )
    .await;
    assert!(fixture.workspace.join("plain.txt").exists());
    let prompt = requests
        .lock()
        .unwrap()
        .last()
        .map(|request| request.system.clone())
        .unwrap();
    assert!(
        prompt.contains(&format!(
            "Working directory: {}",
            fixture.workspace.display()
        )),
        "{prompt}"
    );
    // Its project folder is not checkpointed here (no Git, no setting), and
    // a chat folder's store does not stand in for it.
    assert!(matches!(
        runtime.revert(thread.id, None).await,
        Err(CoreError::Invalid(message)) if message == "snapshots are disabled"
    ));
}

/// The folder a chat home gives a chat of its own: `<today>/chat-<4>`.
fn fresh(fixture: &Fixture, thread: &Thread) -> PathBuf {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let id = thread.id.0.simple().to_string();
    fixture
        .workspace
        .join(today)
        .join(format!("chat-{}", &id[id.len() - 4..]))
}

#[tokio::test]
async fn in_the_chat_home_every_chat_works_in_a_folder_of_its_own() {
    let fixture = Fixture::new();
    let (runtime, _) = fixture.chat_home();
    // A client that names no folder (the TUI, ACP, `run`, an older client,
    // a workflow started on its own): a new one.
    let plain = chat(&runtime, None).await.unwrap();
    assert_eq!(folder(&plain), fresh(&fixture, &plain));
    assert!(folder(&plain).is_dir());
    turn(&runtime, plain.id, &pwd()).await;
    assert_eq!(
        last_result(&runtime, plain.id, "bash").await,
        format!("exit: 0\n{}\n", folder(&plain).display())
    );
    let other = chat(&runtime, None).await.unwrap();
    assert_ne!(other.directory, plain.directory);
    // A chat made under another (a workflow's agents, a task) shares it.
    let child = runtime
        .create_thread(CreateThreadRequest {
            parent_id: Some(plain.id),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(child.directory, plain.directory);
    // A create retried with its id, twice at once: one chat, one folder.
    let id = ThreadId::new();
    let create = || {
        runtime.create_thread(CreateThreadRequest {
            id: Some(id),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
    };
    let (left, right) = tokio::join!(create(), create());
    let (left, right) = (left.unwrap(), right.unwrap());
    assert_eq!((left.id, &left.directory), (right.id, &right.directory));
    let date = folder(&left).parent().unwrap().to_path_buf();
    let named = format!("chat-{}", &id.0.simple().to_string()[28..]);
    assert!(
        std::fs::read_dir(&date)
            .unwrap()
            .filter_map(Result::ok)
            .all(|entry| entry.file_name().to_string_lossy() != format!("{named}-2")),
        "no second folder"
    );
    // The same named twice at once (two clients): one waits for the other.
    let (named_left, named_right) = tokio::join!(
        chat(&runtime, Some("2026-10-24/same")),
        chat(&runtime, Some("2026-10-24/same"))
    );
    assert_ne!(
        named_left.unwrap().directory,
        named_right.unwrap().directory
    );
}

#[tokio::test]
async fn an_imported_chat_in_the_chat_home_gets_a_folder_of_its_own() {
    // Exported from another computer's project, with a folder named in the
    // archive by hand, which is never trusted.
    let elsewhere = Fixture::new();
    let (project, _) = elsewhere.runtime();
    let source = chat(&project, None).await.unwrap();
    turn(&project, source.id, "hello").await;
    let fork = project.fork_thread(source.id, None, None).await.unwrap();
    let mut archive = project
        .export_session_archive(source.id, false)
        .await
        .unwrap();
    for entry in &mut archive.threads {
        entry.thread.directory = Some("/etc".into());
    }
    let fixture = Fixture::new();
    let (runtime, _) = fixture.chat_home();
    runtime.import_session_archive(archive).await.unwrap();
    let imported = runtime.get_thread(source.id).await.unwrap();
    assert_eq!(folder(&imported), fresh(&fixture, &imported));
    assert_eq!(
        runtime.get_thread(fork.id).await.unwrap().directory,
        imported.directory,
        "its fork works there too"
    );
    turn(&runtime, source.id, &pwd()).await;
    assert_eq!(
        last_result(&runtime, source.id, "bash").await,
        format!("exit: 0\n{}\n", folder(&imported).display())
    );
    // Imported into a project, it works in the project folder.
    let another = Fixture::new();
    let (other, _) = another.runtime();
    let archive = project
        .export_session_archive(source.id, false)
        .await
        .unwrap();
    other.import_session_archive(archive).await.unwrap();
    assert_eq!(other.get_thread(source.id).await.unwrap().directory, None);
}

#[tokio::test]
async fn a_chat_stored_without_a_folder_gets_one_before_it_works_in_the_chat_home() {
    let fixture = Fixture::new();
    let database = fixture.root.join("db.sqlite");
    // Stored as one from before chats had folders.
    let legacy = SqliteStore::open(&database)
        .unwrap()
        .create_thread(CreateThreadRequest {
            title: Some("Before".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(legacy.directory, None);
    let (runtime, _) = fixture.chat_home();
    turn(&runtime, legacy.id, &pwd()).await;
    let now = runtime.get_thread(legacy.id).await.unwrap();
    assert_eq!(folder(&now), fresh(&fixture, &now));
    assert_eq!(
        last_result(&runtime, legacy.id, "bash").await,
        format!("exit: 0\n{}\n", folder(&now).display())
    );
    // A person's own command there too.
    let item = runtime.run_shell(legacy.id, "pwd -P".into()).await.unwrap();
    assert!(
        matches!(&item.data, ItemData::ToolResult { output, .. }
            if *output == format!("exit: 0\n{}\n", folder(&now).display())),
        "{:?}",
        item.data
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_link_put_in_place_of_a_chat_folder_refuses_the_chat_s_work() {
    let fixture = Fixture::new();
    let (runtime, _) = fixture.runtime();
    let a = chat(&runtime, Some("2026-10-24/a")).await.unwrap();
    let b = chat(&runtime, Some("2026-10-24/b")).await.unwrap();
    std::fs::write(folder(&b).join("secret.txt"), "B").unwrap();
    for target in [folder(&b), fixture.workspace.join("2026-10-24")] {
        let _ = std::fs::remove_dir_all(folder(&a));
        let _ = std::fs::remove_file(folder(&a));
        std::os::unix::fs::symlink(&target, folder(&a)).unwrap();
        let refused = runtime
            .start_turn(a.id, turn_request(&pwd()))
            .await
            .unwrap_err();
        assert!(
            matches!(&refused, CoreError::Invalid(message) if message.contains("link")),
            "{refused}"
        );
        assert!(
            runtime
                .run_shell(a.id, "cat secret.txt".into())
                .await
                .is_err()
        );
        assert!(runtime.diff_last_turn(a.id).await.is_err());
    }
}

#[tokio::test]
async fn reading_never_makes_a_deleted_chat_folder_again() {
    let fixture = Fixture::new();
    let (runtime, _) = fixture.runtime();
    let thread = chat(&runtime, Some("2026-10-24/read")).await.unwrap();
    turn(
        &runtime,
        thread.id,
        r#"!tool write {"path":"a.txt","content":"a"}"#,
    )
    .await;
    std::fs::remove_dir_all(folder(&thread)).unwrap();
    let _ = runtime.diff_last_turn(thread.id).await;
    let _ = runtime.diff_last_turn_review(thread.id, true, None).await;
    runtime
        .resource_mention_policy(Some(thread.id), None)
        .await
        .unwrap();
    assert!(!folder(&thread).exists(), "a review made the folder again");
    // A turn, which may write, does.
    turn(&runtime, thread.id, &pwd()).await;
    assert!(folder(&thread).is_dir());
}

#[tokio::test]
async fn a_legacy_chat_and_its_child_share_one_folder_in_the_chat_home() {
    let fixture = Fixture::new();
    let database = fixture.root.join("db.sqlite");
    let store = SqliteStore::open(&database).unwrap();
    let parent = store
        .create_thread(CreateThreadRequest {
            title: Some("Before".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let child = store
        .create_thread(CreateThreadRequest {
            parent_id: Some(parent.id),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    drop(store);
    let (runtime, _) = fixture.chat_home();
    // The child works first: the line of chats gets one folder.
    turn(&runtime, child.id, &pwd()).await;
    let child = runtime.get_thread(child.id).await.unwrap();
    let parent = runtime.get_thread(parent.id).await.unwrap();
    assert_eq!(child.directory, parent.directory);
    assert_eq!(folder(&parent), fresh(&fixture, &parent));
    // Reading a chat without a folder (a mention's rules) gives it one too.
    let reader = SqliteStore::open(&database)
        .unwrap()
        .create_thread(CreateThreadRequest {
            title: Some("Reader".into()),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    runtime
        .resource_mention_policy(Some(reader.id), None)
        .await
        .unwrap();
    assert!(
        runtime
            .get_thread(reader.id)
            .await
            .unwrap()
            .directory
            .is_some()
    );
}

#[tokio::test]
async fn a_shared_folder_someone_deleted_is_made_again() {
    let fixture = Fixture::new();
    let (runtime, _) = fixture.chat_home();
    let first = chat(&runtime, Some("2026-10-24/shared")).await.unwrap();
    std::fs::remove_dir_all(folder(&first)).unwrap();
    // A schedule's run shares the chat's folder by its absolute path.
    let run = chat(&runtime, first.directory.as_deref()).await.unwrap();
    assert_eq!(run.directory, first.directory);
    assert!(folder(&run).is_dir());
    // Only a chat folder's name is shared or made.
    for refused in [
        fixture.workspace.join("2026-10-24/.git"),
        fixture.workspace.join("2026-10-24/-rf"),
        fixture.workspace.join("notadate/x"),
    ] {
        assert!(
            chat(&runtime, Some(refused.to_str().unwrap()))
                .await
                .is_err()
        );
        assert!(!refused.exists());
    }
}

/// A server over the fixture's project folder, and its address.
async fn served(
    fixture: &Fixture,
    runtime: AgentRuntime,
) -> (String, haarness_server::ServerHandle) {
    let handle = serve(
        TcpListener::bind("127.0.0.1:0").await.unwrap(),
        "fixture-token",
        ServerState {
            browser: None,
            management: None,
            runtime: Arc::new(runtime),
            mcp: Arc::new(NoMcp),
            skills: Arc::new(NoSkills),
            plugins: None,
            workflows: None,
            workspaces: None,
            workspace_id: None,
            workspace: fixture.workspace.clone(),
            project_trusted: false,
            language: None,
        },
    )
    .await
    .unwrap();
    (format!("http://{}", handle.address), handle)
}

/// The chat home under a repository whose `.gitignore` hides everything (a
/// home folder kept in Git): a chat's search, `@` file search and Undo still
/// see its files, and the repository above does not make its dependency
/// folders searchable.
#[tokio::test]
async fn ignore_files_above_the_chat_home_hide_nothing_from_its_chats() {
    let fixture = Fixture::new();
    std::fs::create_dir(fixture.root.join(".git")).unwrap();
    std::fs::write(fixture.root.join(".gitignore"), "*\n").unwrap();
    let (runtime, _) = fixture.chat_home();
    let a = chat(&runtime, None).await.unwrap();
    turn(
        &runtime,
        a.id,
        r#"!tool write {"path":"plan.md","content":"needle"}"#,
    )
    .await;
    // Undo's checkpoints hold the file.
    let review = runtime.diff_last_turn(a.id).await.unwrap();
    assert_eq!(
        review
            .iter()
            .map(|file| file.file.as_str())
            .collect::<Vec<_>>(),
        ["plan.md"]
    );
    turn(&runtime, a.id, r#"!tool grep {"pattern":"needle"}"#).await;
    let found = last_result(&runtime, a.id, "grep").await;
    assert!(found.contains("plan.md"), "{found}");
    // A person's own search there (a workflow's too) as well.
    let found = runtime
        .run_direct_tool(
            a.id,
            "grep",
            json!({"pattern": "needle"}),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .output
        .text;
    assert!(found.contains("plan.md"), "{found}");
    // A search of a subfolder leaves out what the chat's folder leaves out,
    // the model's and a person's alike.
    std::fs::create_dir_all(folder(&a).join("sub")).unwrap();
    std::fs::write(folder(&a).join(".ignore"), "*.log\n").unwrap();
    std::fs::write(folder(&a).join("sub/a.log"), "needle").unwrap();
    std::fs::write(folder(&a).join("sub/b.txt"), "needle").unwrap();
    // As grep writes it on this system.
    let kept = std::path::Path::new("sub")
        .join("b.txt")
        .display()
        .to_string();
    turn(
        &runtime,
        a.id,
        r#"!tool grep {"pattern":"needle","path":"sub"}"#,
    )
    .await;
    let found = last_result(&runtime, a.id, "grep").await;
    assert!(found.contains(&kept) && !found.contains("a.log"), "{found}");
    let found = runtime
        .run_direct_tool(
            a.id,
            "grep",
            json!({"pattern": "needle", "path": "sub"}),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .output
        .text;
    assert!(found.contains(&kept) && !found.contains("a.log"), "{found}");

    std::fs::create_dir_all(folder(&a).join("node_modules/dep")).unwrap();
    std::fs::write(folder(&a).join("node_modules/dep/plan.js"), "").unwrap();
    let (base, _handle) = served(&fixture, runtime).await;
    let found: Vec<String> = reqwest::Client::new()
        .get(format!(
            "{base}/v1/files/search?q=plan&limit=50&threadId={}",
            a.id
        ))
        .bearer_auth("fixture-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(found, ["plan.md"]);
}

#[tokio::test]
async fn the_file_apis_work_in_a_chat_s_own_folder_when_asked_for_that_chat() {
    let fixture = Fixture::new();
    let (runtime, _) = fixture.chat_home();
    let a = chat(&runtime, Some("2026-10-24/a")).await.unwrap();
    let b = chat(&runtime, Some("2026-10-24/b")).await.unwrap();
    let plain = runtime
        .create_thread(CreateThreadRequest {
            parent_id: Some(a.id),
            provider: Some("echo".into()),
            model: Some("echo-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    std::fs::create_dir_all(folder(&a).join("docs")).unwrap();
    std::fs::write(folder(&a).join("docs/target-notes.md"), "mine").unwrap();
    for index in 0..60 {
        std::fs::write(folder(&b).join(format!("target-{index}.md")), "theirs").unwrap();
    }
    std::fs::write(folder(&b).join("secret.txt"), "B").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(folder(&b).join("secret.txt"), folder(&a).join("peek.txt"))
            .unwrap();
        std::os::unix::fs::symlink(folder(&b), folder(&a).join("sibling")).unwrap();
    }
    let (base, _handle) = served(&fixture, runtime).await;
    let client = reqwest::Client::new();
    let get = |path: String| {
        client
            .get(format!("{base}/v1{path}"))
            .bearer_auth("fixture-token")
            .send()
    };

    // Search: the chat's own file, relative to its folder, whatever its
    // siblings hold.
    let found: Vec<String> = get(format!("/files/search?q=target&limit=50&threadId={}", a.id))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(found, ["docs/target-notes.md"]);
    // `./` completion starts at the chat's folder too.
    let completed: Vec<String> = get(format!("/files/search?q=./do&threadId={}", a.id))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(completed, ["./docs/"]);
    // Without it, as before: the whole chat home, named from it.
    let everywhere: Vec<String> = get("/files/search?q=target&limit=50".into())
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        everywhere
            .iter()
            .all(|path| path.starts_with("2026-10-24/")),
        "{everywhere:?}"
    );

    // Listing and reading: relative paths, and the folder they are relative to.
    let listed: Value = get(format!("/files/list?path=docs&threadId={}", a.id))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["entries"][0]["path"], "docs/target-notes.md");
    assert_eq!(listed["directory"], json!(a.directory));
    let read: Value = get(format!(
        "/files/content?path=docs/target-notes.md&threadId={}",
        a.id
    ))
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(
        (read["content"].as_str(), read["path"].as_str()),
        (Some("mine"), Some("docs/target-notes.md"))
    );
    assert_eq!(read["directory"], json!(a.directory));
    let unscoped: Value = get("/files/list?path=2026-10-24/a/docs".into())
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        unscoped["entries"][0]["path"],
        "2026-10-24/a/docs/target-notes.md"
    );
    assert!(unscoped.get("directory").is_none());

    // A link inside the chat's folder to another chat's is refused like an
    // outside path, and is no folder to open.
    #[cfg(unix)]
    {
        for path in ["peek.txt", "sibling/secret.txt"] {
            let refused = get(format!("/files/content?path={path}&threadId={}", a.id))
                .await
                .unwrap();
            assert_eq!(refused.status(), reqwest::StatusCode::BAD_REQUEST, "{path}");
        }
        let refused = get(format!("/files/list?path=sibling&threadId={}", a.id))
            .await
            .unwrap();
        assert_eq!(refused.status(), reqwest::StatusCode::BAD_REQUEST);
        let top: Value = get(format!("/files/list?threadId={}", a.id))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let sibling = top["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == "sibling")
            .unwrap();
        assert_eq!(sibling["directory"], false);
        let write = client
            .put(format!("{base}/v1/files/content?threadId={}", a.id))
            .bearer_auth("fixture-token")
            .json(&json!({"path": "sibling/planted.txt", "content": "x", "revision": hash("")}))
            .send()
            .await
            .unwrap();
        assert_eq!(write.status(), reqwest::StatusCode::BAD_REQUEST);
        assert!(!folder(&b).join("planted.txt").exists());
    }

    // Writing, making, renaming and deleting happen in the chat's folder.
    let written = client
        .put(format!("{base}/v1/files/content?threadId={}", a.id))
        .bearer_auth("fixture-token")
        .json(&json!({"path": "new.txt", "content": "hello", "revision": hash("")}))
        .send()
        .await
        .unwrap();
    assert!(written.status().is_success());
    assert_eq!(
        std::fs::read_to_string(folder(&a).join("new.txt")).unwrap(),
        "hello"
    );
    let action = |body: Value| {
        client
            .post(format!("{base}/v1/files/action?threadId={}", a.id))
            .bearer_auth("fixture-token")
            .json(&body)
            .send()
    };
    assert!(
        action(json!({"action": "mkdir", "path": "made"}))
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert!(folder(&a).join("made").is_dir());
    assert!(
        action(json!({"action": "rename", "path": "new.txt", "target": "made/renamed.txt"}))
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert!(folder(&a).join("made/renamed.txt").is_file());
    assert!(
        action(json!({"action": "delete", "path": "made/renamed.txt", "confirm": true}))
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert!(!folder(&a).join("made/renamed.txt").exists());
    assert_eq!(
        action(json!({"action": "rename", "path": "made", "target": "../b/made"}))
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::BAD_REQUEST
    );

    // A chat made under another lists its parent's folder.
    let child_listed: Value = get(format!("/files/list?threadId={}", plain.id))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        child_listed["directory"],
        json!(a.directory),
        "a child shares its parent's folder"
    );

    // An unknown chat is 404, a malformed id 400.
    assert_eq!(
        get(format!("/files/list?threadId={}", ThreadId::new()))
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );
    assert_eq!(
        get("/files/search?q=x&threadId=not-an-id".into())
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::BAD_REQUEST
    );

    // A published chat may ask only for chats of its own tree.
    let scoped = |path: String| {
        client
            .get(format!("{base}/v1{path}"))
            .bearer_auth("fixture-token")
            .header("x-haarness-remote-thread", a.id.to_string())
            .send()
    };
    assert!(
        scoped(format!("/files/list?threadId={}", a.id))
            .await
            .unwrap()
            .status()
            .is_success()
    );
    for path in [
        format!("/files/list?threadId={}", b.id),
        format!("/files/content?path=secret.txt&threadId={}", b.id),
        format!("/files/search?q=target&threadId={}", b.id),
    ] {
        assert_eq!(
            scoped(path.clone()).await.unwrap().status(),
            reqwest::StatusCode::FORBIDDEN,
            "{path}"
        );
    }
    let planted = client
        .put(format!("{base}/v1/files/content?threadId={}", b.id))
        .bearer_auth("fixture-token")
        .header("x-haarness-remote-thread", a.id.to_string())
        .json(&json!({"path": "planted.txt", "content": "x", "revision": hash("")}))
        .send()
        .await
        .unwrap();
    assert_eq!(planted.status(), reqwest::StatusCode::FORBIDDEN);
    assert!(!folder(&b).join("planted.txt").exists());
    // A chat folder someone deleted is not made again by the file APIs.
    std::fs::remove_dir_all(folder(&b)).unwrap();
    for path in [
        format!("/files/list?threadId={}", b.id),
        format!("/files/content?path=secret.txt&threadId={}", b.id),
    ] {
        assert_eq!(
            get(path.clone()).await.unwrap().status(),
            reqwest::StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    let written = client
        .put(format!("{base}/v1/files/content?threadId={}", b.id))
        .bearer_auth("fixture-token")
        .json(&json!({"path": "new.txt", "content": "x", "revision": hash("")}))
        .send()
        .await
        .unwrap();
    assert_eq!(written.status(), reqwest::StatusCode::NOT_FOUND);
    assert!(!folder(&b).exists());
}

/// A file's revision as the file API computes it.
fn hash(content: &str) -> String {
    use sha2::Digest as _;
    format!("{:x}", sha2::Sha256::digest(content.as_bytes()))
}

#[tokio::test]
async fn the_file_apis_of_a_project_chat_are_the_project_folder_s_as_before() {
    let fixture = Fixture::new();
    let (runtime, _) = fixture.runtime();
    let thread = chat(&runtime, None).await.unwrap();
    std::fs::create_dir_all(fixture.workspace.join("src")).unwrap();
    std::fs::write(fixture.workspace.join("src/main.rs"), "fn main() {}").unwrap();
    let (base, _handle) = served(&fixture, runtime).await;
    let client = reqwest::Client::new();
    for query in [String::new(), format!("&threadId={}", thread.id)] {
        let listed: Value = client
            .get(format!("{base}/v1/files/list?path=src{query}"))
            .bearer_auth("fixture-token")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(listed["entries"][0]["path"], "src/main.rs", "{query}");
        assert!(listed.get("directory").is_none(), "{query}");
        let found: Vec<String> = client
            .get(format!("{base}/v1/files/search?q=main{query}"))
            .bearer_auth("fixture-token")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(found, ["src/main.rs"], "{query}");
    }
}

#[tokio::test]
async fn search_finds_a_chat_s_files_past_a_big_dependency_folder_and_honours_ignore_files() {
    let fixture = Fixture::new();
    let (runtime, _) = fixture.chat_home();
    let thread = chat(&runtime, Some("2026-10-24/deps")).await.unwrap();
    let root = folder(&thread);
    // More entries than a search walks, in a folder that is no repository.
    for package in 0..300 {
        let directory = root.join(format!("node_modules/pkg{package:03}"));
        std::fs::create_dir_all(&directory).unwrap();
        for file in 0..200 {
            std::fs::File::create(directory.join(format!("f{file}.js"))).unwrap();
        }
    }
    std::fs::create_dir_all(root.join("zz-src")).unwrap();
    std::fs::write(root.join("zz-src/app.ts"), "x").unwrap();
    std::fs::create_dir_all(root.join("generated")).unwrap();
    std::fs::write(root.join("generated/app.ts"), "x").unwrap();
    std::fs::write(root.join(".gitignore"), "generated/\n").unwrap();
    // A build folder outside Git is left out, one in a repository cloned
    // into the folder is the project's.
    std::fs::create_dir_all(root.join("dist")).unwrap();
    std::fs::write(root.join("dist/bundle.js"), "x").unwrap();
    std::fs::create_dir_all(root.join("cloned/dist")).unwrap();
    git_fixture_command(&root.join("cloned"), ["init", "-q", "-b", "main"]);
    std::fs::write(root.join("cloned/dist/index.js"), "x").unwrap();
    let (base, _handle) = served(&fixture, runtime).await;
    let client = reqwest::Client::new();
    let search = |query: &str| {
        let url = format!(
            "{base}/v1/files/search?q={query}&limit=50&threadId={}",
            thread.id
        );
        let client = client.clone();
        async move {
            client
                .get(url)
                .bearer_auth("fixture-token")
                .send()
                .await
                .unwrap()
                .json::<Vec<String>>()
                .await
                .unwrap()
        }
    };
    let started = std::time::Instant::now();
    assert_eq!(
        search("app.ts").await,
        ["zz-src/app.ts"],
        "the ignored copy is left out"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    // Named, the dependency folder is searched.
    assert!(
        search("node_modules/pkg007/f19")
            .await
            .contains(&"node_modules/pkg007/f19.js".to_owned())
    );
    assert!(search("bundle.js").await.is_empty());
    assert_eq!(search("dist/bundle").await, ["dist/bundle.js"]);
    assert_eq!(search("index.js").await, ["cloned/dist/index.js"]);
    // `.ignore` counts as `.gitignore` does.
    std::fs::remove_file(root.join(".gitignore")).unwrap();
    std::fs::write(root.join(".ignore"), "generated/\n").unwrap();
    assert_eq!(search("app.ts").await, ["zz-src/app.ts"]);
}

#[tokio::test]
async fn search_in_a_git_project_finds_files_committed_under_build_folder_names() {
    let fixture = Fixture::new();
    let (runtime, _) = fixture.runtime();
    let root = fixture.workspace.clone();
    let committed = [
        "dist/index.js",
        "build/package/Dockerfile",
        "target/thumbv7em/memory.x",
        "cmd/dist/main.go",
        "pkg/venv/activate.go",
    ];
    git_fixture_command(&root, ["init", "-q", "-b", "main"]);
    for path in committed {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "x").unwrap();
    }
    // What the project ignores stays out.
    std::fs::create_dir_all(root.join("out")).unwrap();
    std::fs::write(root.join("out/index.js"), "x").unwrap();
    std::fs::write(root.join(".gitignore"), "out/\n").unwrap();
    git_fixture_command(&root, ["add", "-A"]);
    git_fixture_command(
        &root,
        [
            "-c",
            "user.email=api@example.invalid",
            "-c",
            "user.name=Haarness API Test",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "base",
        ],
    );
    let (base, _handle) = served(&fixture, runtime).await;
    let client = reqwest::Client::new();
    let search = |query: &str| {
        let url = format!("{base}/v1/files/search?q={query}&limit=50");
        let client = client.clone();
        async move {
            client
                .get(url)
                .bearer_auth("fixture-token")
                .send()
                .await
                .unwrap()
                .json::<Vec<String>>()
                .await
                .unwrap()
        }
    };
    for (query, path) in [
        ("index.js", "dist/index.js"),
        ("Dockerfile", "build/package/Dockerfile"),
        ("memory.x", "target/thumbv7em/memory.x"),
        ("main.go", "cmd/dist/main.go"),
        ("activate.go", "pkg/venv/activate.go"),
    ] {
        assert_eq!(search(query).await, [path], "{query}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn files_can_be_written_into_a_folder_one_may_write_in_but_not_read() {
    use std::os::unix::fs::PermissionsExt as _;
    /// Gives the folder its reading back, also when an assertion fails.
    struct Readable(PathBuf);
    impl Drop for Readable {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }
    let fixture = Fixture::new();
    let (runtime, _) = fixture.runtime();
    let drop_box = fixture.workspace.join("drop");
    std::fs::create_dir_all(&drop_box).unwrap();
    std::fs::set_permissions(&drop_box, std::fs::Permissions::from_mode(0o300)).unwrap();
    let readable = Readable(drop_box.clone());
    let (base, _handle) = served(&fixture, runtime).await;
    let client = reqwest::Client::new();
    let written = client
        .put(format!("{base}/v1/files/content"))
        .bearer_auth("fixture-token")
        .json(&json!({"path": "drop/note.txt", "content": "dropped", "revision": hash("")}))
        .send()
        .await
        .unwrap();
    assert!(
        written.status().is_success(),
        "{}",
        written.text().await.unwrap()
    );
    let action = |body: Value| {
        client
            .post(format!("{base}/v1/files/action"))
            .bearer_auth("fixture-token")
            .json(&body)
            .send()
    };
    for body in [
        json!({"action": "mkdir", "path": "drop/inner"}),
        json!({"action": "rename", "path": "drop/note.txt", "target": "drop/renamed.txt"}),
    ] {
        let answer = action(body.clone()).await.unwrap();
        assert!(
            answer.status().is_success(),
            "{body}: {}",
            answer.text().await.unwrap()
        );
    }
    drop(readable);
    assert_eq!(
        std::fs::read_to_string(drop_box.join("renamed.txt")).unwrap(),
        "dropped"
    );
    assert!(drop_box.join("inner").is_dir());
    assert!(!drop_box.join("note.txt").exists());
}
