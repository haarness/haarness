//! A chat's own temp folder: its commands get it as `TMPDIR`, `TMP` and
//! `TEMP`, and its shell and file tools reach it without asking about a
//! folder outside the project, in Review and in Auto (where the reviewer is
//! not asked). Another chat's folder still asks. Its child agents, its
//! background commands, a person's `!` command and a workflow's share it,
//! a `shell.env` hook does not move it, a person's deny still refuses it, and
//! the prompt names no path. Archiving keeps it; the cleanup at start takes
//! away what archived and unknown chats left once nothing used it for a week.
use super::*;
use haarness_core::{HookCall, HookDecision, HookPort};
use haarness_protocol::{
    ApprovalMode, ApprovalModeSetting, ItemData, PermissionReply, PermissionReplyDecision as Reply,
    SetApprovalModeRequest, ThreadId, UpdateThreadRequest,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("haarness-chat-temp-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(path.join("project")).unwrap();
        Self(dunce::canonicalize(&path).unwrap())
    }
    fn project(&self) -> PathBuf {
        self.0.join("project")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A `shell.env` hook that gives the temp folder variables another value.
struct Elsewhere;
#[async_trait]
impl HookPort for Elsewhere {
    async fn pre_tool_use(&self, _: &HookCall, _: &str, _: &Value) -> HookDecision {
        HookDecision::default()
    }
    async fn post_tool_use(
        &self,
        _: &HookCall,
        _: &str,
        _: &Value,
        _: &str,
        _: bool,
    ) -> HookDecision {
        HookDecision::default()
    }
    async fn user_prompt_submit(&self, _: &HookCall, _: &str) -> HookDecision {
        HookDecision::default()
    }
    async fn permission_request(
        &self,
        _: &HookCall,
        _: &str,
        _: &[String],
        _: Option<&[String]>,
        _: &Value,
    ) -> HookDecision {
        HookDecision::default()
    }
    async fn shell_env(&self, _: ThreadId, _: &Path) -> BTreeMap<String, String> {
        [
            ("TMPDIR", "/elsewhere"),
            ("TMP", "/elsewhere"),
            ("TEMP", "/elsewhere"),
            ("HOOK_ENV", "ok"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
    }
}

/// A `shell.env` hook that gives commands `.0` as their home folder, whose
/// login profile sets the temp folder variables.
struct ProfileHome(PathBuf);
#[async_trait]
impl HookPort for ProfileHome {
    async fn pre_tool_use(&self, _: &HookCall, _: &str, _: &Value) -> HookDecision {
        HookDecision::default()
    }
    async fn post_tool_use(
        &self,
        _: &HookCall,
        _: &str,
        _: &Value,
        _: &str,
        _: bool,
    ) -> HookDecision {
        HookDecision::default()
    }
    async fn user_prompt_submit(&self, _: &HookCall, _: &str) -> HookDecision {
        HookDecision::default()
    }
    async fn permission_request(
        &self,
        _: &HookCall,
        _: &str,
        _: &[String],
        _: Option<&[String]>,
        _: &Value,
    ) -> HookDecision {
        HookDecision::default()
    }
    async fn shell_env(&self, _: ThreadId, _: &Path) -> BTreeMap<String, String> {
        BTreeMap::from([("HOME".to_owned(), self.0.display().to_string())])
    }
}

/// A runtime under Haarness's default rules, in Review.
fn runtime(workspace: &Path, hooks: Option<Arc<dyn HookPort>>) -> (AgentRuntime, Capturing) {
    runtime_with(workspace, hooks, RuntimeConfig::default())
}

/// [`runtime`] with `config`'s rules.
fn runtime_with(
    workspace: &Path,
    hooks: Option<Arc<dyn HookPort>>,
    config: RuntimeConfig,
) -> (AgentRuntime, Capturing) {
    let provider = Capturing::default();
    let runtime = AgentRuntime::with_config(
        Arc::new(SqliteStore::in_memory().unwrap()),
        Arc::new(StaticProviderRegistry::new(vec![
            Arc::new(provider.clone()) as Arc<dyn ModelProvider>,
        ])),
        Arc::new(NativeToolRegistry::new(workspace.to_owned())),
        EventBus::new(2048),
        workspace.to_owned(),
        String::new(),
        RuntimeConfig {
            approval_mode: ApprovalMode::Review,
            ..config
        },
    );
    let runtime = match hooks {
        Some(hooks) => runtime.with_hooks(hooks),
        None => runtime,
    };
    (runtime, provider)
}

fn call(name: &str, arguments: Value) -> String {
    format!("!tool {name} {arguments}")
}

fn bash(command: &str) -> String {
    call("bash", json!({ "command": command }))
}

/// A turn of the model that makes `prompt`'s call and ends without asking
/// anyone anything.
async fn runs_unasked(runtime: &AgentRuntime, thread: ThreadId, prompt: &str) {
    let turn = runtime
        .start_turn(thread, turn_request(prompt))
        .await
        .unwrap();
    let done = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            assert!(
                runtime.pending_permissions().is_empty(),
                "{prompt} asked: {:?}",
                runtime.pending_permissions()
            );
            let current = runtime.turn(turn.id).await.unwrap();
            if !matches!(current.status, TurnStatus::Queued | TurnStatus::Running) {
                return current;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the turn did not finish");
    assert_eq!(done.status, TurnStatus::Completed, "{prompt}");
}

/// A turn of the model that makes `prompt`'s call, which asks about a folder
/// outside the project; the person refuses it.
async fn asks_outside(runtime: &AgentRuntime, thread: ThreadId, prompt: &str) {
    let turn = runtime
        .start_turn(thread, turn_request(prompt))
        .await
        .unwrap();
    wait_pending_permissions(runtime, 1).await;
    let pending = runtime.pending_permissions().remove(0);
    assert_eq!(pending.permission, "external_directory", "{prompt}");
    runtime
        .reply_permission(
            &pending.permission_id,
            PermissionReply {
                decision: Reply::Reject,
                feedback: None,
            },
        )
        .await
        .unwrap();
    wait_done(runtime, &turn).await;
}

/// The results of the calls recorded in `thread`, oldest first.
async fn results(runtime: &AgentRuntime, thread: ThreadId) -> Vec<(String, String, bool)> {
    runtime
        .items(thread, None)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item.data {
            ItemData::ToolResult {
                name,
                output,
                is_error,
                ..
            } => Some((name, output, is_error)),
            _ => None,
        })
        .collect()
}

async fn last_output(runtime: &AgentRuntime, thread: ThreadId) -> String {
    let (_, output, is_error) = results(runtime, thread).await.pop().unwrap();
    assert!(!is_error, "{output}");
    output
}

async fn new_chat(runtime: &AgentRuntime) -> ThreadId {
    echo_thread(runtime).await.id
}

async fn archive(runtime: &AgentRuntime, thread: ThreadId, archived: bool) {
    runtime
        .update_thread(
            thread,
            UpdateThreadRequest {
                title: None,
                archived: Some(archived),
                agent: None,
                provider: None,
                model: None,
                ultracode: None,
            },
        )
        .await
        .unwrap();
}

/// The review requests the provider received.
fn reviews(provider: &Capturing) -> usize {
    provider
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| {
            request
                .output_schema
                .as_ref()
                .and_then(|schema| schema.get("description"))
                .and_then(Value::as_str)
                == Some(haarness_core::auto_review::VERDICT_SCHEMA_DESCRIPTION)
        })
        .count()
}

/// Sets `path`'s modification time back a month.
fn age(path: &Path) {
    let old = SystemTime::now() - Duration::from_secs(30 * 24 * 60 * 60);
    std::fs::File::open(path)
        .unwrap()
        .set_modified(old)
        .unwrap();
}

/// The record beside a chat's folder.
fn record(folder: &Path) -> PathBuf {
    folder.with_file_name(format!(
        "{}.chat",
        folder.file_name().unwrap().to_string_lossy()
    ))
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777
}

#[tokio::test]
async fn a_chat_s_commands_get_its_own_temp_folder_and_reach_it_without_asking() {
    let fixture = Fixture::new();
    let (runtime, provider) = runtime(&fixture.project(), None);
    let thread = new_chat(&runtime).await;
    assert_eq!(haarness_core::chat_temp::existing(thread), None);
    runs_unasked(
        &runtime,
        thread,
        &bash(
            r#"printf '%s|%s|%s|' "$TMPDIR" "$TMP" "$TEMP" && echo made > "$TMPDIR/out.txt" && cat $TMPDIR/out.txt && mkdir -p ${TMPDIR}/build/deep && cp "$TMPDIR/out.txt" "$TMP/copy.txt" && rm -rf "$TMPDIR/build""#,
        ),
    )
    .await;
    let folder = haarness_core::chat_temp::existing(thread).expect("made for the command");
    let shown = folder.display().to_string();
    assert_eq!(
        last_output(&runtime, thread).await,
        format!("exit: 0\n{shown}|{shown}|{shown}|made\n")
    );
    assert_eq!(mode(&folder), 0o700);
    assert!(folder.join("copy.txt").is_file());
    assert!(!folder.join("build").exists());
    // Written out in full, as `echo $TMPDIR` shows it, and through the link
    // a Mac's `/tmp` is.
    runs_unasked(
        &runtime,
        thread,
        &bash(&format!(
            "cd {shown} && echo again > again.txt && cat again.txt"
        )),
    )
    .await;
    assert!(folder.join("again.txt").is_file());
    if let Ok(spelled) = folder.strip_prefix("/private") {
        runs_unasked(
            &runtime,
            thread,
            &bash(&format!("cat /{}/again.txt", spelled.display())),
        )
        .await;
    }

    // The file tools.
    let notes = folder.join("notes.md").display().to_string();
    runs_unasked(
        &runtime,
        thread,
        &call("write", json!({ "path": notes, "content": "temp notes\n" })),
    )
    .await;
    runs_unasked(&runtime, thread, &call("read", json!({ "path": notes }))).await;
    assert!(last_output(&runtime, thread).await.contains("temp notes"));
    runs_unasked(
        &runtime,
        thread,
        &call(
            "edit",
            json!({ "path": notes, "old": "temp", "new": "kept" }),
        ),
    )
    .await;
    runs_unasked(&runtime, thread, &call("list", json!({ "path": shown }))).await;
    assert!(last_output(&runtime, thread).await.contains("notes.md"));
    runs_unasked(
        &runtime,
        thread,
        &call("grep", json!({ "pattern": "kept", "path": shown })),
    )
    .await;
    assert!(last_output(&runtime, thread).await.contains("notes.md"));
    runs_unasked(
        &runtime,
        thread,
        &call("glob", json!({ "pattern": "*.md", "path": shown })),
    )
    .await;
    assert!(last_output(&runtime, thread).await.contains("notes.md"));

    // The prompt names the folder by its variable alone, so every chat of
    // the project sends the same one.
    let other = new_chat(&runtime).await;
    runs_unasked(&runtime, other, &bash("true")).await;
    let systems: Vec<String> = provider
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.system.clone())
        .collect();
    assert!(systems[0].contains("which shell commands find in TMPDIR"));
    assert!(!systems[0].contains(&shown));
    assert!(systems.iter().all(|system| *system == systems[0]));
    assert_ne!(
        haarness_core::chat_temp::existing(other),
        Some(folder.clone())
    );
    haarness_core::chat_temp::remove(thread);
    haarness_core::chat_temp::remove(other);
}

#[tokio::test]
async fn another_chat_s_temp_folder_and_a_way_out_of_its_own_still_ask() {
    let fixture = Fixture::new();
    let (runtime, _) = runtime(&fixture.project(), None);
    let first = new_chat(&runtime).await;
    runs_unasked(&runtime, first, &bash(r#"echo first > "$TMPDIR/a.txt""#)).await;
    let theirs = haarness_core::chat_temp::existing(first).unwrap();
    let second = new_chat(&runtime).await;
    runs_unasked(&runtime, second, &bash("true")).await;
    let mine = haarness_core::chat_temp::existing(second).unwrap();
    std::os::unix::fs::symlink(&theirs, mine.join("link")).unwrap();
    let name = theirs.file_name().unwrap().to_string_lossy().into_owned();
    for prompt in [
        bash(&format!("cat {}/a.txt", theirs.display())),
        bash(&format!("echo x > {}/b.txt", theirs.display())),
        bash(&format!(r#"cat "$TMPDIR/../{name}/a.txt""#)),
        bash(r#"cat "$TMPDIR/link/a.txt""#),
        bash("echo x > $TMPDIR/../x.txt"),
        call(
            "write",
            json!({ "path": theirs.join("b.txt"), "content": "x" }),
        ),
        call("read", json!({ "path": mine.join("link/a.txt") })),
    ] {
        asks_outside(&runtime, second, &prompt).await;
    }
    assert!(!theirs.join("b.txt").exists());
    haarness_core::chat_temp::remove(first);
    haarness_core::chat_temp::remove(second);
}

#[tokio::test]
async fn in_auto_the_reviewer_is_not_asked_about_the_chat_s_own_temp_folder() {
    let fixture = Fixture::new();
    let (runtime, provider) = runtime(&fixture.project(), None);
    let thread = new_chat(&runtime).await;
    runtime
        .set_approval_mode(
            thread,
            SetApprovalModeRequest {
                mode: ApprovalModeSetting::Auto,
                revision: None,
            },
        )
        .await
        .unwrap();
    runs_unasked(
        &runtime,
        thread,
        &bash(r#"python3 -c pass > "$TMPDIR/log.txt" 2>&1; cat "$TMPDIR/log.txt""#),
    )
    .await;
    let folder = haarness_core::chat_temp::existing(thread).unwrap();
    runs_unasked(
        &runtime,
        thread,
        &call(
            "write",
            json!({ "path": folder.join("plan.md"), "content": "x" }),
        ),
    )
    .await;
    assert_eq!(reviews(&provider), 0);
    // A folder outside the project still goes to the reviewer, which allows it here.
    let elsewhere = fixture.0.join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    runs_unasked(
        &runtime,
        thread,
        &bash(&format!("echo x > {}/x.txt", elsewhere.display())),
    )
    .await;
    assert_eq!(reviews(&provider), 1);
    haarness_core::chat_temp::remove(thread);
}

#[tokio::test]
async fn child_agents_background_commands_a_person_and_a_workflow_share_the_chat_s_folder() {
    const SHOW: &str = r#"printf %s "$TMPDIR""#;
    let fixture = Fixture::new();
    let (runtime, _) = runtime(&fixture.project(), Some(Arc::new(Elsewhere)));
    let thread = new_chat(&runtime).await;
    runs_unasked(
        &runtime,
        thread,
        &bash(r#"printf '%s|%s|%s|%s' "$TMPDIR" "$TMP" "$TEMP" "$HOOK_ENV""#),
    )
    .await;
    let folder = haarness_core::chat_temp::existing(thread).unwrap();
    let shown = folder.display().to_string();
    // A `shell.env` hook's values for them give way; its other ones stay.
    assert_eq!(
        last_output(&runtime, thread).await,
        format!("exit: 0\n{shown}|{shown}|{shown}|ok")
    );

    // A child agent.
    runs_unasked(&runtime, thread, &tasks_prompt(1, SHOW)).await;
    let child = runtime
        .list_threads(false)
        .await
        .unwrap()
        .into_iter()
        .find(|chat| chat.parent_id == Some(thread))
        .unwrap()
        .id;
    assert_eq!(
        last_output(&runtime, child).await,
        format!("exit: 0\n{shown}")
    );
    assert_eq!(haarness_core::chat_temp::existing(child), None);

    // A command in the background.
    runs_unasked(
        &runtime,
        thread,
        &call(
            "bash",
            json!({
                "command": r#"printf %s "$TMPDIR" > "$TMPDIR/background.txt""#,
                "run_in_background": true,
            }),
        ),
    )
    .await;
    let written = folder.join("background.txt");
    tokio::time::timeout(Duration::from_secs(20), async {
        while std::fs::read_to_string(&written).ok().as_deref() != Some(shown.as_str()) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the background command did not write in the chat's folder");

    // A person's own `!` command and a workflow's.
    let item = runtime.run_shell(thread, SHOW.into()).await.unwrap();
    assert!(
        matches!(&item.data, ItemData::ToolResult { output, is_error: false, .. }
            if *output == format!("exit: 0\n{shown}")),
        "{:?}",
        item.data
    );
    let result = runtime
        .run_workflow_tool(
            thread,
            "bash",
            json!({ "command": format!(r#"{SHOW} && cat "$TMPDIR/background.txt""#) }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.output.metadata["stdout"], format!("{shown}{shown}"));
    haarness_core::chat_temp::remove(thread);
}

/// A login profile that sets `TMPDIR`, `TMP` and `TEMP` runs after the
/// command's environment is set; the command still gets the chat's folder,
/// so what it does with `$TMPDIR`, which asked nothing, stays there.
#[tokio::test]
async fn a_login_profile_cannot_move_the_chat_s_temp_folder() {
    let fixture = Fixture::new();
    let home = fixture.0.join("home");
    std::fs::create_dir_all(home.join("mytmp")).unwrap();
    std::fs::write(home.join("mytmp/keep.txt"), "keep").unwrap();
    std::fs::write(
        home.join(".profile"),
        "export TMPDIR=\"$HOME/mytmp\" TMP=\"$HOME/mytmp\" TEMP=\"$HOME/mytmp\"\n",
    )
    .unwrap();
    let (runtime, _) = runtime(
        &fixture.project(),
        Some(Arc::new(ProfileHome(home.clone()))),
    );
    let thread = new_chat(&runtime).await;
    runs_unasked(
        &runtime,
        thread,
        &bash(
            r#"printf '%s|%s|%s|' "$TMPDIR" "$TMP" "$TEMP"; rm -rf "$TMPDIR"/*; echo made > "$TMPDIR/made.txt""#,
        ),
    )
    .await;
    let folder = haarness_core::chat_temp::existing(thread).unwrap();
    let shown = folder.display().to_string();
    assert_eq!(
        last_output(&runtime, thread).await,
        format!("exit: 0\n{shown}|{shown}|{shown}|")
    );
    assert!(home.join("mytmp/keep.txt").is_file());
    assert!(!home.join("mytmp/made.txt").exists());
    assert!(folder.join("made.txt").is_file());
    // A login shell inside the line reads the profile again: its `$TMPDIR`
    // is checked as before, and asks.
    asks_outside(&runtime, thread, &bash(r#"bash -lc 'rm -rf "$TMPDIR"/*'"#)).await;
    assert!(home.join("mytmp/keep.txt").is_file());
    haarness_core::chat_temp::remove(thread);
}

/// A login profile that keeps `TMPDIR` read-only stops only a command whose
/// check read it as the chat's folder, with a line that says what to change;
/// any other command runs as before.
#[tokio::test]
async fn a_read_only_temp_variable_stops_only_commands_that_use_the_folder() {
    let fixture = Fixture::new();
    let home = fixture.0.join("home");
    std::fs::create_dir_all(home.join("mytmp")).unwrap();
    std::fs::write(home.join(".profile"), "readonly TMPDIR=\"$HOME/mytmp\"\n").unwrap();
    let (runtime, _) = runtime(
        &fixture.project(),
        Some(Arc::new(ProfileHome(home.clone()))),
    );
    let thread = new_chat(&runtime).await;
    runs_unasked(&runtime, thread, &bash("echo hello")).await;
    assert_eq!(last_output(&runtime, thread).await, "exit: 0\nhello\n");
    runs_unasked(&runtime, thread, &bash(r#"echo x > "$TMPDIR/x.txt""#)).await;
    let (_, output, _) = results(&runtime, thread).await.pop().unwrap();
    assert!(output.starts_with("exit: 126\n"), "{output}");
    assert!(output.contains("Remove readonly"), "{output}");
    assert!(!home.join("mytmp/x.txt").exists());
    haarness_core::chat_temp::remove(thread);
}

#[tokio::test]
async fn a_person_s_deny_still_refuses_the_chat_s_own_temp_folder() {
    let fixture = Fixture::new();
    // The folder that holds every chat's folder, here.
    let probe = ThreadId::new();
    let chats = haarness_core::chat_temp::folder(probe)
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    haarness_core::chat_temp::remove(probe);
    let mut config = RuntimeConfig::default();
    config.base_rules.push(haarness_core::PermissionRule {
        pattern: format!("external_directory:{}/*", chats.display()),
        decision: haarness_core::PermissionDecision::Deny,
    });
    let (runtime, _) = runtime_with(&fixture.project(), None, config);
    let thread = new_chat(&runtime).await;
    runs_unasked(&runtime, thread, &bash(r#"echo x > "$TMPDIR/x.txt""#)).await;
    let folder = haarness_core::chat_temp::existing(thread).unwrap();
    runs_unasked(
        &runtime,
        thread,
        &call(
            "write",
            json!({ "path": folder.join("y.txt"), "content": "y" }),
        ),
    )
    .await;
    let refused = results(&runtime, thread).await;
    assert_eq!(refused.len(), 2, "{refused:?}");
    for (_, output, is_error) in refused {
        assert!(
            is_error && output.contains("is denied by permission rule 'external_directory:"),
            "{output}"
        );
    }
    assert!(!folder.join("x.txt").exists() && !folder.join("y.txt").exists());
    // The project is the project, as before.
    runs_unasked(&runtime, thread, &bash("echo z > z.txt")).await;
    assert!(fixture.project().join("z.txt").is_file());
    haarness_core::chat_temp::remove(thread);
}

#[tokio::test]
async fn archiving_a_chat_keeps_its_temp_folder_for_when_it_comes_back() {
    let fixture = Fixture::new();
    let (runtime, _) = runtime(&fixture.project(), None);
    let thread = new_chat(&runtime).await;
    runs_unasked(
        &runtime,
        thread,
        &bash(r#"mkdir -p "$TMPDIR/build" && echo x > "$TMPDIR/build/out.o""#),
    )
    .await;
    let folder = haarness_core::chat_temp::existing(thread).unwrap();
    archive(&runtime, thread, true).await;
    // What a removal in the background would have taken by now.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(folder.join("build/out.o").is_file());
    // Lately used: the cleanup at start keeps it too.
    runtime.sweep_chat_temp_folders().await;
    assert!(folder.join("build/out.o").is_file());
    archive(&runtime, thread, false).await;
    runs_unasked(&runtime, thread, &bash(r#"cat "$TMPDIR/build/out.o""#)).await;
    assert_eq!(last_output(&runtime, thread).await, "exit: 0\nx\n");
    haarness_core::chat_temp::remove(thread);
}

#[tokio::test]
async fn the_cleanup_at_start_takes_away_what_archived_and_unknown_chats_left_unused() {
    let fixture = Fixture::new();
    let (runtime, _) = runtime(&fixture.project(), None);
    let live = new_chat(&runtime).await;
    let (archived, lately_archived) = (new_chat(&runtime).await, new_chat(&runtime).await);
    for chat in [archived, lately_archived] {
        archive(&runtime, chat, true).await;
    }
    // Another server's chats, or a test's whose data folder is gone.
    let (unused, recent) = (ThreadId::new(), ThreadId::new());
    let mut folders = Vec::new();
    for chat in [live, archived, lately_archived, unused, recent] {
        let folder = haarness_core::chat_temp::folder(chat).unwrap();
        std::fs::write(folder.join("file"), "x").unwrap();
        folders.push(folder);
    }
    for folder in [&folders[0], &folders[1], &folders[3]] {
        for path in [folder.join("file"), folder.clone(), record(folder)] {
            age(&path);
        }
    }
    assert_eq!(runtime.sweep_chat_temp_folders().await, 2);
    let kept = |index: usize| folders[index].join("file").is_file();
    let gone = |index: usize| !folders[index].exists() && !record(&folders[index]).exists();
    assert!(kept(0), "a chat it keeps, however old");
    assert!(gone(1), "archived and unused for a week");
    assert!(kept(2), "archived lately");
    assert!(gone(3), "unknown and unused for a week");
    assert!(kept(4), "unknown, used lately");
    for chat in [live, lately_archived, recent] {
        haarness_core::chat_temp::remove(chat);
    }
}
