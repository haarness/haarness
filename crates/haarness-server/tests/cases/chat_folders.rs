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
            RuntimeConfig::default(),
        )
        .with_folder_snapshots(Arc::new(
            haarness_store::SqliteSnapshotFolders::open(&database).unwrap(),
        ));
        (runtime, requests)
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
    std::fs::create_dir_all(fixture.root.join("outside")).unwrap();
    std::os::unix::fs::symlink(fixture.root.join("outside"), fixture.workspace.join("out"))
        .unwrap();
    std::os::unix::fs::symlink(".", fixture.workspace.join("here")).unwrap();
    for refused in [
        "../escape",
        "/tmp/x",
        fixture.root.join("outside").to_str().unwrap(),
        "",
        "a//b",
        "a/./b",
        "out/chat",
        "here",
    ] {
        let error = chat(&runtime, Some(refused)).await.unwrap_err();
        assert!(matches!(error, CoreError::Invalid(_)), "{refused}: {error}");
    }
    assert!(!fixture.root.join("escape").exists());
    assert!(
        !fixture.root.join("outside/chat").exists(),
        "nothing made through the link"
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
        .json(&json!({"provider": "echo", "model": "echo-1", "directory": "day/chat"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(folder(&created), fixture.workspace.join("day/chat"));
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
}

#[tokio::test]
async fn a_chat_works_in_its_folder_and_a_sibling_s_folder_is_outside() {
    let fixture = Fixture::new();
    let (runtime, requests) = fixture.runtime();
    let a = chat(&runtime, Some("day/a")).await.unwrap();
    let b = chat(&runtime, Some("day/b")).await.unwrap();

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
        "day/a/page.html"
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
    let thread = chat(&runtime, Some("day/attach")).await.unwrap();
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
    let a = chat(&runtime, Some("day/a")).await.unwrap();
    let b = chat(&runtime, Some("day/b")).await.unwrap();
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
    let parent = chat(&runtime, Some("day/parent")).await.unwrap();
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
            directory: Some("day/plan".into()),
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
    let thread = chat(&runtime, Some("day/gone")).await.unwrap();
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
