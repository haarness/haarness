//! Stream shell output to disk while retaining only a bounded tail in memory.
use haarness_core::{CoreError, CoreResult, ToolOutput};
use haarness_protocol::ThreadId;
use serde_json::json;
use std::{collections::BTreeMap, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use tokio_util::sync::CancellationToken;

struct ProcessGroup(u32);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Ok(pid) = i32::try_from(self.0) {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
        #[cfg(windows)]
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &self.0.to_string()])
            .output();
    }
}

/// `env` adds or overrides variables for this one invocation (`shell.env`
/// hooks); the parent environment is otherwise inherited, minus the server's
/// private variables (`haarness_core::private_env_names`). `agent` is the
/// chat a command the agent runs is for (`None` for a person's own command):
/// see [`mark_agent`].
pub(crate) async fn run(
    command: &str,
    workdir: &Path,
    timeout_ms: u64,
    cancel: &CancellationToken,
    directory: &Path,
    env: &BTreeMap<String, String>,
    agent: Option<ThreadId>,
) -> CoreResult<ToolOutput> {
    let error = |e: std::io::Error| CoreError::Tool(e.to_string());
    let mut command = haarness_ripgrep::shell_command(command).map_err(error)?;
    command
        .current_dir(dunce::simplified(workdir))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in haarness_core::private_env_names() {
        command.env_remove(name);
    }
    // A person's own command is not marked, also when this server holds
    // Haarness's own markers (a `haarness serve` an agent's command started).
    if agent.is_none() {
        for name in haarness_core::own_agent_markers(std::env::var_os("AI_AGENT").as_deref()) {
            command.env_remove(name);
        }
    }
    // Without a locale (Desktop opened from Finder) a command would read 日本
    // as six characters, as a terminal would; a hook's own LANG still wins.
    if let Some(locale) = haarness_pty::missing_locale() {
        command.env("LANG", locale);
    }
    let env: Vec<_> = env
        .iter()
        .filter(|(key, value)| {
            !key.is_empty() && !key.contains(['=', '\0']) && !value.contains('\0')
        })
        .collect();
    command.envs(env.iter().copied());
    if let Some(thread) = agent {
        mark_agent(&mut command, thread, &env);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    tokio::fs::create_dir_all(directory).await.map_err(error)?;
    let path = directory.join(format!("tool_{}.txt", uuid::Uuid::now_v7()));
    let mut file = tokio::fs::File::create(&path).await.map_err(error)?;
    let mut child = Command::from(command)
        .kill_on_drop(true)
        .spawn()
        .map_err(error)?;
    let _group = ProcessGroup(
        child
            .id()
            .ok_or_else(|| CoreError::Tool("shell process did not start".into()))?,
    );
    // Stopped by the next server should this one die before it can.
    let _recorded = haarness_core::process_groups::recorded(child.id());
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let mut tail = Vec::new();
    let mut stdout_tail = Vec::new();
    let mut total = 0usize;
    let mut lines = 0usize;
    let execution = async {
        // Heap buffers keep this future small enough to await unboxed.
        let mut out = vec![0u8; 8192];
        let mut err = vec![0u8; 8192];
        let mut out_done = false;
        let mut err_done = false;
        let mut status = None;
        while !out_done || !err_done || status.is_none() {
            let bytes = tokio::select! {
                value = stdout.read(&mut out), if !out_done => {
                    let n = value.map_err(error)?; out_done = n == 0;
                    stdout_tail.extend_from_slice(&out[..n]);
                    if stdout_tail.len() > 50 * 1024 { stdout_tail.drain(..stdout_tail.len() - 50 * 1024); }
                    &out[..n]
                },
                value = stderr.read(&mut err), if !err_done => {
                    let n = value.map_err(error)?; err_done = n == 0; &err[..n]
                },
                value = child.wait(), if status.is_none() => { status = Some(value.map_err(error)?); continue; }
            };
            total += bytes.len();
            lines += newlines(bytes);
            file.write_all(bytes).await.map_err(error)?;
            tail.extend_from_slice(bytes);
            if tail.len() > 50 * 1024 {
                tail.drain(..tail.len() - 50 * 1024);
            }
        }
        file.flush().await.map_err(error)?;
        Ok::<_, CoreError>(status.expect("wait completed").code().unwrap_or(-1))
    };
    let deadline = async {
        if timeout_ms == 0 {
            std::future::pending::<()>().await;
        } else {
            tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
        }
    };
    let result = tokio::select! {
        biased;
        () = cancel.cancelled() => Err(CoreError::Cancelled),
        () = deadline => Err(CoreError::Tool(format!("command timed out after {timeout_ms} ms; output saved to {}", path.display()))),
        result = execution => result,
    };
    let exit = result?;
    let truncated = total > 50 * 1024 || lines > 2000;
    let text = String::from_utf8_lossy(&tail);
    let preview = text
        .lines()
        .rev()
        .take(2000)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    let stdout_text = String::from_utf8_lossy(&stdout_tail);
    let stdout_preview = stdout_text
        .lines()
        .rev()
        .take(2000)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    let mut metadata = json!({"exit":exit,"truncated":truncated,"stdout": if stdout_text.lines().count() > 2000 { stdout_preview.as_str() } else { stdout_text.as_ref() }});
    let notice = if truncated {
        metadata["outputPath"] = json!(path);
        format!("Full output saved to: {}\n", path.display())
    } else {
        drop(file);
        let _ = tokio::fs::remove_file(&path).await;
        String::new()
    };
    Ok(ToolOutput {
        text: format!(
            "exit: {exit}\n{notice}{}",
            if truncated {
                preview.as_str()
            } else {
                text.as_ref()
            }
        ),
        metadata,
    })
}

/// Marks a command the agent runs as an agent's (`AGENT`, `AI_AGENT`, and
/// `HAARNESS`), for scripts and tools that adapt to one, and names the chat
/// it runs for (`HAARNESS_SESSION_ID`). They win over the server's own
/// environment, which a `haarness` started by another chat's command inherits
/// with that chat's id, and over `env` (`shell.env` hooks and extensions). An
/// `AI_AGENT` already set, there or in the server's environment, names an
/// outer agent and is kept, unless it is Haarness's own. A person's own
/// command is not marked.
/// The newlines in `bytes`; a plain count, not worth a dependency.
#[allow(clippy::naive_bytecount)]
fn newlines(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&byte| byte == b'\n').count()
}

fn mark_agent(command: &mut std::process::Command, thread: ThreadId, env: &[(&String, &String)]) {
    // What the command would get otherwise: `env`'s value, else the server's.
    let given = |name: &str| {
        env.iter()
            .rev()
            .find(|(key, _)| {
                if cfg!(windows) {
                    key.eq_ignore_ascii_case(name)
                } else {
                    key.as_str() == name
                }
            })
            .map(|(_, value)| std::ffi::OsString::from(value.as_str()))
            .or_else(|| std::env::var_os(name))
            .filter(|value| !value.is_empty())
    };
    let id = thread.to_string();
    // `AI_AGENT=haarness` (this server started in an agent's command, say)
    // is Haarness's own marker, not an outer agent's.
    let outer = given("AI_AGENT").is_some_and(|value| value != haarness_core::AI_AGENT_VALUE);
    let stale = given(haarness_core::SESSION_ID_ENV).is_some_and(|value| value != id.as_str());
    command
        .env("AGENT", "1")
        .env("HAARNESS", "1")
        .env(haarness_core::SESSION_ID_ENV, &id);
    if !outer {
        command.env("AI_AGENT", haarness_core::AI_AGENT_VALUE);
    }
    // The values themselves are not logged: neither the outer marker kept nor
    // the id replaced.
    tracing::info!(
        thread_id = %thread,
        outer_marker_kept = outer,
        stale_id_replaced = stale,
        "agent shell environment set"
    );
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    fn fixture() -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("haarness-shell-parity-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    #[tokio::test]
    async fn streaming_preserves_exit_and_stdout_while_bounding_large_output() {
        let root = fixture();
        let output = run(
            "printf stdout; printf stderr >&2; exit 7",
            &root,
            0,
            &CancellationToken::new(),
            &root,
            &BTreeMap::new(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(output.metadata["exit"], 7);
        assert_eq!(output.metadata["stdout"], "stdout");
        assert!(output.text.contains("stderr"));
        let env = BTreeMap::from([
            ("HOOK_SHELL_VAR".to_owned(), "from-hook".to_owned()),
            ("BAD=KEY".to_owned(), "skipped".to_owned()),
        ]);
        let output = run(
            "printf '%s' \"$HOOK_SHELL_VAR\"",
            &root,
            0,
            &CancellationToken::new(),
            &root,
            &env,
            None,
        )
        .await
        .unwrap();
        assert_eq!(output.metadata["stdout"], "from-hook");
        let output = run(
            "awk 'BEGIN {for(i=0;i<50000;i++) print i}'",
            &root,
            600_001,
            &CancellationToken::new(),
            &root,
            &BTreeMap::new(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(output.metadata["truncated"], true);
        assert!(output.text.len() < 52 * 1024);
        assert!(output.text.contains("49999"));
        let saved = output.metadata["outputPath"].as_str().unwrap();
        assert_eq!(
            std::fs::read_to_string(saved).unwrap().lines().count(),
            50000
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn a_command_reads_multibyte_text_as_characters_without_a_locale() {
        let root = fixture();
        let script = "x='日本'; printf '%s %s' \"${#x}\" \"$LANG\"";
        let lang = |output: &ToolOutput| output.metadata["stdout"].as_str().unwrap().to_owned();
        if haarness_pty::missing_locale().is_some() {
            // This process has no locale, as Desktop opened from Finder has none.
            let output = run(
                script,
                &root,
                0,
                &CancellationToken::new(),
                &root,
                &BTreeMap::new(),
                None,
            )
            .await
            .unwrap();
            assert!(lang(&output).starts_with("2 "), "{output:?}");
        }
        // A hook's LANG wins over the default.
        let env = BTreeMap::from([("LANG".to_owned(), "C".to_owned())]);
        let output = run(
            "printf '%s' \"$LANG\"",
            &root,
            0,
            &CancellationToken::new(),
            &root,
            &env,
            None,
        )
        .await
        .unwrap();
        assert_eq!(lang(&output), "C");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn zero_deadline_is_cancellable_and_timeout_kills_descendants() {
        for timeout in [0, 3000] {
            let root = fixture();
            let cancel = CancellationToken::new();
            let token = cancel.clone();
            let work = root.clone();
            let task = tokio::spawn(async move {
                run(
                    "sh -c 'sleep 5; touch escaped' & printf ready > ready; wait",
                    &work,
                    timeout,
                    &token,
                    &work,
                    &BTreeMap::new(),
                    None,
                )
                .await
            });
            tokio::time::timeout(Duration::from_secs(5), async {
                while !root.join("ready").exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            if timeout == 0 {
                cancel.cancel();
            }
            let error = task.await.unwrap().unwrap_err();
            if timeout == 0 {
                assert!(matches!(error, CoreError::Cancelled));
            } else {
                assert!(error.to_string().contains("timed out"));
            }
            tokio::time::sleep(Duration::from_millis(5500)).await;
            assert!(!root.join("escaped").exists());
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    /// The cases of [`agent_marker_case`] run in a child test process whose
    /// own environment (the server's, which a command inherits) holds
    /// `inherited`, with the markers and the server's token unset otherwise.
    fn in_server_environment(case: &str, inherited: &[(&str, &str)]) {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "shell::tests::agent_marker_case", "--nocapture"])
            .env("HAARNESS_SHELL_MARKER_CASE", case);
        for name in [
            "AGENT",
            "HAARNESS",
            "AI_AGENT",
            "HAARNESS_SESSION_ID",
            "HAARNESS_SERVER_TOKEN",
        ] {
            command.env_remove(name);
        }
        let output = command.envs(inherited.iter().copied()).output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains(&format!("case {case} passed")),
            "{case}: {stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// The formatted INFO events of this thread while it lives.
    #[derive(Clone, Default)]
    struct Logs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Logs {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Logs {
        fn capture() -> (Self, tracing::subscriber::DefaultGuard) {
            let logs = Self::default();
            let writer = logs.clone();
            let subscriber = tracing_subscriber::fmt()
                .with_writer(move || writer.clone())
                .with_ansi(false)
                .without_time()
                .with_max_level(tracing::Level::INFO)
                .finish();
            (logs, tracing::subscriber::set_default(subscriber))
        }
        fn marked(&self) -> Vec<String> {
            String::from_utf8_lossy(&self.0.lock().unwrap())
                .lines()
                .filter(|line| line.contains("agent shell environment set"))
                .map(str::to_owned)
                .collect()
        }
    }

    const SHOW: &str = r#"printf '%s|%s|%s|%s' "${AGENT-unset}" "${HAARNESS-unset}" "${AI_AGENT-unset}" "${HAARNESS_SESSION_ID-unset}""#;

    async fn stdout(
        root: &Path,
        script: &str,
        env: &BTreeMap<String, String>,
        agent: Option<ThreadId>,
    ) -> String {
        let output = run(script, root, 0, &CancellationToken::new(), root, env, agent)
            .await
            .unwrap();
        assert_eq!(output.metadata["exit"], 0, "{output:?}");
        output.metadata["stdout"].as_str().unwrap().to_owned()
    }

    /// One case of the tests below, in the environment they give it.
    #[tokio::test]
    async fn agent_marker_case() {
        let Ok(case) = std::env::var("HAARNESS_SHELL_MARKER_CASE") else {
            return;
        };
        let root = fixture();
        let (logs, _guard) = Logs::capture();
        let thread = ThreadId::new();
        let none = BTreeMap::new();
        let line = |outer: bool, stale: bool| {
            format!(
                "agent shell environment set thread_id={thread} outer_marker_kept={outer} stale_id_replaced={stale}"
            )
        };
        match case.as_str() {
            "stale" => {
                // The id another chat's command left in the server's environment
                // is replaced, and the markers it set to 0 are set again; an
                // empty AI_AGENT names no outer agent.
                assert_eq!(
                    stdout(&root, SHOW, &none, Some(thread)).await,
                    format!("1|1|haarness|{thread}")
                );
                assert_eq!(logs.marked().len(), 1);
                assert!(
                    logs.marked()[0].ends_with(&line(false, true)),
                    "{:?}",
                    logs.marked()
                );
                // A person's own command is not marked, and no chat id reaches it:
                // the server's AGENT and HAARNESS are left out too.
                assert_eq!(stdout(&root, SHOW, &none, None).await, "unset|unset||unset");
                assert_eq!(logs.marked().len(), 1);
            }
            "own" => {
                // This server started in another chat's agent command (a
                // `haarness serve`, say): its AI_AGENT=haarness is Haarness's
                // own marker, not an outer agent's, and a person's command
                // gets none of that command's markers.
                assert_eq!(
                    stdout(&root, SHOW, &none, Some(thread)).await,
                    format!("1|1|haarness|{thread}")
                );
                assert!(
                    logs.marked()[0].ends_with(&line(false, true)),
                    "{:?}",
                    logs.marked()
                );
                assert_eq!(
                    stdout(&root, SHOW, &none, None).await,
                    "unset|unset|unset|unset"
                );
            }
            "outer" => {
                // An agent that runs Haarness keeps its own marker.
                assert_eq!(
                    stdout(&root, SHOW, &none, Some(thread)).await,
                    format!("1|1|outer-agent|{thread}")
                );
                assert!(
                    logs.marked()[0].ends_with(&line(true, false)),
                    "{:?}",
                    logs.marked()
                );
                assert_eq!(
                    stdout(&root, SHOW, &none, None).await,
                    "unset|unset|outer-agent|unset"
                );
            }
            "hook" => {
                // A `shell.env` hook (or an extension) adds variables, and an
                // AI_AGENT it sets is kept as an outer marker, but it cannot mark
                // the command as not an agent's or name another chat.
                let env = BTreeMap::from(
                    [
                        ("AGENT", "0"),
                        ("HAARNESS", "0"),
                        ("AI_AGENT", "hook-agent"),
                        ("HAARNESS_SESSION_ID", "hook"),
                        ("HOOK_ENV", "ok"),
                    ]
                    .map(|(name, value)| (name.to_owned(), value.to_owned())),
                );
                let script = format!(r#"{SHOW}; printf '|%s' "$HOOK_ENV""#);
                assert_eq!(
                    stdout(&root, &script, &env, Some(thread)).await,
                    format!("1|1|hook-agent|{thread}|ok")
                );
                assert!(
                    logs.marked()[0].ends_with(&line(true, true)),
                    "{:?}",
                    logs.marked()
                );
                // A person's command gets the hook's variables as they are.
                assert_eq!(
                    stdout(&root, &script, &env, None).await,
                    "0|0|hook-agent|hook|ok"
                );
                // An empty AI_AGENT from a hook names no outer agent either.
                let env = BTreeMap::from([("AI_AGENT".to_owned(), String::new())]);
                assert_eq!(
                    stdout(&root, SHOW, &env, Some(thread)).await,
                    format!("1|1|haarness|{thread}")
                );
            }
            "clean" => {
                // The new variables hold the fixed markers and the chat id, nothing
                // else, and the id is in no other variable.
                let listing = stdout(&root, "env", &none, Some(thread)).await;
                let lines: Vec<_> = listing.lines().collect();
                let named = |test: &dyn Fn(&str) -> bool| {
                    let mut found: Vec<_> =
                        lines.iter().copied().filter(|line| test(line)).collect();
                    found.sort_unstable();
                    found
                };
                assert_eq!(
                    named(&|line| line.contains(&thread.to_string())),
                    [format!("HAARNESS_SESSION_ID={thread}")]
                );
                assert_eq!(
                    named(&|line| ["AGENT=", "AI_AGENT=", "HAARNESS="]
                        .iter()
                        .any(|start| line.starts_with(start))),
                    ["AGENT=1", "AI_AGENT=haarness", "HAARNESS=1"]
                );
                assert!(!listing.contains("server-secret"), "{listing}");
                assert!(!listing.contains("OPENCODE"), "{listing}");
                // One line per command, the values left out.
                stdout(&root, "true", &none, Some(thread)).await;
                assert_eq!(logs.marked().len(), 2, "{:?}", logs.marked());
                for logged in logs.marked() {
                    assert!(logged.ends_with(&line(false, false)), "{logged}");
                    assert!(logged.starts_with(" INFO"), "{logged}");
                }
                assert_eq!(
                    stdout(&root, SHOW, &none, None).await,
                    "unset|unset|unset|unset"
                );
            }
            other => panic!("unknown case {other}"),
        }
        std::fs::remove_dir_all(root).unwrap();
        println!("case {case} passed");
    }

    #[test]
    fn an_agent_command_replaces_a_stale_chat_id_and_a_person_s_gets_none() {
        in_server_environment(
            "stale",
            &[
                ("AGENT", "0"),
                ("HAARNESS", "0"),
                ("AI_AGENT", ""),
                ("HAARNESS_SESSION_ID", "stale"),
            ],
        );
    }

    #[test]
    fn a_server_started_in_an_agent_s_command_marks_its_commands_as_its_own() {
        in_server_environment(
            "own",
            &[
                ("AGENT", "1"),
                ("HAARNESS", "1"),
                ("AI_AGENT", "haarness"),
                ("HAARNESS_SESSION_ID", "another-chat"),
            ],
        );
    }

    #[test]
    fn an_agent_command_keeps_an_outer_ai_agent_marker() {
        in_server_environment("outer", &[("AGENT", "1"), ("AI_AGENT", "outer-agent")]);
    }

    #[test]
    fn a_shell_env_hook_cannot_unmark_an_agent_command_or_name_another_chat() {
        in_server_environment("hook", &[]);
    }

    #[test]
    fn the_markers_carry_only_fixed_values_and_the_chat_id_logged_once_per_command() {
        in_server_environment("clean", &[("HAARNESS_SERVER_TOKEN", "server-secret")]);
    }
}
