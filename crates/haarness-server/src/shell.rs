//! Run shell commands: their output streams to disk and its end stays in
//! memory, while a task of its own reads it, so a command can outlive the
//! call that started it (an agent's command in the background).
use haarness_core::{
    CommandOutput, CommandProcess, CommandRequest, CommandRun, CoreError, CoreResult,
    OUTPUT_SAVED_LIMIT, ProcessControl, ProcessEnd, ShellCall, StopSignal, ToolOutput,
};
use haarness_protocol::ThreadId;
use serde_json::json;
use std::fmt::Write as _;
use std::{
    collections::BTreeMap,
    path::Path,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command, sync::watch};
use tokio_util::sync::CancellationToken;

/// Kills the command's process group when its reading ends, whatever is
/// left of it.
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
        taskkill(self.0, true);
    }
}

/// `taskkill` on `pid` and its tree, with `/F` when `force`, off the async
/// threads when there is a runtime to hand it to: it waits for the tree.
#[cfg(windows)]
fn taskkill(pid: u32, force: bool) {
    let run = move || {
        let pid = pid.to_string();
        let mut command = std::process::Command::new("taskkill");
        if force {
            command.arg("/F");
        }
        let _ = command.args(["/T", "/PID", pid.as_str()]).output();
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => drop(handle.spawn_blocking(run)),
        Err(_) => run(),
    }
}

/// Signals for a running command: on Unix to its process group (it leads
/// one of its own); on Windows `taskkill` on its tree, without `/F` (a
/// request to close) for an interrupt or terminate, with it for a kill.
struct GroupControl {
    leader: u32,
    /// The group is still ours: its leader has not been reaped, or what it
    /// left running may still be in it.
    live: AtomicBool,
}

impl ProcessControl for GroupControl {
    fn signal(&self, signal: StopSignal) {
        if !self.live.load(Ordering::SeqCst) {
            return;
        }
        #[cfg(unix)]
        if let Ok(pid) = i32::try_from(self.leader) {
            use nix::sys::signal::Signal;
            let signal = match signal {
                StopSignal::Interrupt => Signal::SIGINT,
                StopSignal::Terminate => Signal::SIGTERM,
                StopSignal::Kill => Signal::SIGKILL,
            };
            let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), signal);
        }
        // A console program does not heed a request to close: on Windows
        // the interrupt and terminate steps of a stop do nothing to one,
        // and the kill after them ends it.
        #[cfg(windows)]
        taskkill(self.leader, signal == StopSignal::Kill);
    }
}

/// How long the reading goes on after the command itself ended while
/// something it started still holds its output open, quiet that long or at
/// most that long in all, before what is left is stopped.
const LEFTOVER_QUIET: Duration = Duration::from_millis(250);
const LEFTOVER_MOST: Duration = Duration::from_secs(2);
/// How long the reading goes on once what was left is stopped.
const AFTER_LEFTOVERS: Duration = Duration::from_millis(500);
/// The output an agent's or a person's call shows: its last 50 KiB and
/// 2000 lines.
const SHOWN_BYTES: usize = 50 * 1024;
const SHOWN_LINES: usize = 2000;

/// Holds back the start of a character whose other bytes have not been
/// read yet, so stdout and stderr interleaved never split one.
#[derive(Default)]
struct Characters(Vec<u8>);

impl Characters {
    fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut all = std::mem::take(&mut self.0);
        all.extend_from_slice(bytes);
        let whole = haarness_core::whole_end(&all);
        self.0 = all.split_off(whole);
        all
    }

    fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.0)
    }
}

/// Counts kept besides the output: stdout alone, for a definition's
/// snippet, and the lines of all of it.
#[derive(Default)]
struct Stats {
    stdout_tail: Vec<u8>,
    stdout_total: usize,
    lines: usize,
}

/// A started command and what its call reads of it when it ends.
struct Spawned {
    process: CommandProcess,
    stats: Arc<parking_lot::Mutex<Stats>>,
}

/// `env` adds or overrides variables for this one invocation (`shell.env`
/// hooks); the parent environment is otherwise inherited, minus the server's
/// private variables (`haarness_core::private_env_names`). `agent` is the
/// chat a command the agent runs is for (`None` for a person's own command):
/// see [`mark_agent`].
async fn spawn(
    command: &str,
    workdir: &Path,
    save_limit: u64,
    directory: &Path,
    env: &BTreeMap<String, String>,
    agent: Option<ThreadId>,
) -> CoreResult<Spawned> {
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
    let (path, file) = crate::data_folder::new_private_file(directory, "tool")
        .await
        .map_err(error)?;
    let mut child = Command::from(command)
        .kill_on_drop(true)
        .spawn()
        .map_err(error)?;
    let leader = child
        .id()
        .ok_or_else(|| CoreError::Tool("shell process did not start".into()))?;
    let group = ProcessGroup(leader);
    // Stopped by the next server should this one die before it can.
    let recorded = haarness_core::process_groups::recorded(Some(leader));
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let output = Arc::new(CommandOutput::new(path, file, save_limit));
    let stats = Arc::new(parking_lot::Mutex::new(Stats::default()));
    let control = Arc::new(GroupControl {
        leader,
        live: AtomicBool::new(true),
    });
    let (end, ended) = watch::channel(None);
    tokio::spawn(pump(Pump {
        child,
        stdout,
        stderr,
        output: Arc::clone(&output),
        stats: Arc::clone(&stats),
        end,
        control: Arc::clone(&control),
        group,
        recorded,
    }));
    Ok(Spawned {
        process: CommandProcess {
            output,
            end: ended,
            control,
        },
        stats,
    })
}

/// What reads a running command, owning its process.
struct Pump {
    child: tokio::process::Child,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    output: Arc<CommandOutput>,
    stats: Arc<parking_lot::Mutex<Stats>>,
    end: watch::Sender<Option<ProcessEnd>>,
    control: Arc<GroupControl>,
    group: ProcessGroup,
    recorded: haarness_core::process_groups::Recorded,
}

/// Read the command's output until it has ended and nothing holds its
/// output open, then say how it ended. A command that ends while
/// something it started (`server &`) still runs and holds its output does
/// not wait for that: what is left is stopped.
async fn pump(mut pump: Pump) {
    // Heap buffers keep this future small.
    let mut out = vec![0u8; 8192];
    let mut err = vec![0u8; 8192];
    let mut out_open = true;
    let mut err_open = true;
    let mut out_characters = Characters::default();
    let mut err_characters = Characters::default();
    let mut status = None;
    let mut ended_at = None;
    let mut heard_at = tokio::time::Instant::now();
    let mut stopped_leftovers: Option<tokio::time::Instant> = None;
    let mut leftovers = false;
    let mut leftovers_running = false;
    let mut failure = None;
    while out_open || err_open || status.is_none() {
        // Quiet counts from the command's end or its latest output after
        // it, whichever is later: output it printed long before it ended
        // says nothing about what it left.
        let wake = match (ended_at, stopped_leftovers) {
            (_, Some(stopped)) => Some(stopped + AFTER_LEFTOVERS),
            (Some(ended), None) => {
                Some((heard_at.max(ended) + LEFTOVER_QUIET).min(ended + LEFTOVER_MOST))
            }
            (None, None) => None,
        };
        let sleep = async {
            match wake {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        // The command's end first, so output that never stops cannot hide
        // it; then reads, so a pipe that closed with the command is seen
        // closed before the quiet time can be taken for something holding
        // it; the quiet time last.
        let (bytes, from_stdout) = tokio::select! {
            biased;
            waited = pump.child.wait(), if status.is_none() => {
                match waited {
                    Ok(waited) => status = Some(waited),
                    Err(error) => {
                        failure = Some(error.to_string());
                        break;
                    }
                }
                ended_at = Some(tokio::time::Instant::now());
                continue;
            }
            read = pump.stdout.read(&mut out), if out_open => match read {
                Ok(0) => { out_open = false; continue; }
                Ok(n) => (&out[..n], true),
                Err(error) => { failure = Some(error.to_string()); out_open = false; continue; }
            },
            read = pump.stderr.read(&mut err), if err_open => match read {
                Ok(0) => { err_open = false; continue; }
                Ok(n) => (&err[..n], false),
                Err(error) => { failure = Some(error.to_string()); err_open = false; continue; }
            },
            () = sleep => {
                if stopped_leftovers.is_some() {
                    // Something outside the group still holds the output
                    // open (a process that left it): stop reading.
                    leftovers_running = true;
                    break;
                }
                leftovers = true;
                pump.control.signal(StopSignal::Kill);
                stopped_leftovers = Some(tokio::time::Instant::now());
                continue;
            }
        };
        heard_at = tokio::time::Instant::now();
        // Something that keeps printing after the command ended (`yes &`)
        // never lets the quiet time come: stop it at the most.
        if stopped_leftovers.is_none()
            && ended_at.is_some_and(|ended| heard_at >= ended + LEFTOVER_MOST)
        {
            leftovers = true;
            pump.control.signal(StopSignal::Kill);
            stopped_leftovers = Some(heard_at);
        }
        {
            let mut counts = pump.stats.lock();
            counts.lines += newlines(bytes);
            if from_stdout {
                counts.stdout_total += bytes.len();
                counts.stdout_tail.extend_from_slice(bytes);
                if counts.stdout_tail.len() > SHOWN_BYTES {
                    let cut = counts.stdout_tail.len() - SHOWN_BYTES;
                    counts.stdout_tail.drain(..cut);
                }
            }
        }
        let whole = if from_stdout {
            out_characters.push(bytes)
        } else {
            err_characters.push(bytes)
        };
        if let Err(error) = pump.output.append(&whole).await {
            failure = Some(error.to_string());
            break;
        }
    }
    for rest in [out_characters.finish(), err_characters.finish()] {
        let _ = pump.output.append(&rest).await;
    }
    if let Err(error) = pump.output.finish().await {
        failure.get_or_insert(error.to_string());
    }
    if failure.is_some() || status.is_none() {
        pump.control.signal(StopSignal::Kill);
    }
    let (exit_code, signal) = status.map_or((None, None), |status| {
        (status.code(), terminating_signal(status))
    });
    pump.control.live.store(false, Ordering::SeqCst);
    drop(pump.group);
    drop(pump.recorded);
    pump.end.send_replace(Some(ProcessEnd {
        exit_code,
        signal,
        leftovers,
        leftovers_running,
        error: failure,
    }));
}

/// The signal that ended the command, by name.
fn terminating_signal(status: std::process::ExitStatus) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal().map(|number| {
            nix::sys::signal::Signal::try_from(number).map_or_else(
                |_| format!("signal {number}"),
                |signal| signal.as_str().to_owned(),
            )
        })
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

/// A person's own command, a workflow's, a definition's snippet: run in the
/// foreground until it ends, its deadline (`timeout_ms`, 0 for none) or the
/// stop of `cancel`. See [`spawn`] for the rest.
pub(crate) async fn run(
    command: &str,
    workdir: &Path,
    timeout_ms: u64,
    cancel: &CancellationToken,
    directory: &Path,
    env: &BTreeMap<String, String>,
    agent: Option<ThreadId>,
) -> CoreResult<ToolOutput> {
    // A command with no deadline can print until it is stopped (`npm run
    // dev`, `tail -f`): its saved output keeps only its start.
    let save_limit = if timeout_ms == 0 {
        OUTPUT_SAVED_LIMIT
    } else {
        u64::MAX
    };
    let spawned = spawn(command, workdir, save_limit, directory, env, agent).await?;
    let process = &spawned.process;
    let mut end = process.end.clone();
    let deadline = async {
        if timeout_ms == 0 {
            std::future::pending::<()>().await;
        } else {
            tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
        }
    };
    let ended = tokio::select! {
        biased;
        () = cancel.cancelled() => Err(CoreError::Cancelled),
        () = deadline => Err(CoreError::Tool(format!(
            "command timed out after {timeout_ms} ms; output saved to {}",
            process.output.path().display()
        ))),
        ended = end.wait_for(Option::is_some) => {
            Ok(ended.ok().and_then(|ended| ended.clone()).unwrap_or_default())
        }
    };
    let end = match ended {
        Ok(end) => end,
        Err(error) => {
            process.control.signal(StopSignal::Kill);
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                process.end.clone().wait_for(Option::is_some),
            )
            .await;
            return Err(error);
        }
    };
    if let Some(error) = &end.error {
        return Err(CoreError::Tool(error.clone()));
    }
    Ok(ended_output(&spawned, &end, None).await)
}

/// An agent's command in its turn (`call`): registered so a client follows
/// it, and run in the background, by `run_in_background` or by moving
/// there while it runs. See [`spawn`] and [`ShellCall::run`].
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_call(
    call: &ShellCall,
    request: CommandRequest,
    cancel: &CancellationToken,
    directory: &Path,
    env: &BTreeMap<String, String>,
    agent: Option<ThreadId>,
) -> CoreResult<ToolOutput> {
    call.check_start(request.background)?;
    // A deadline in the foreground saves all of the output; without one
    // (or in the background) only its start.
    let save_limit = if call.saves_whole_output(&request) {
        u64::MAX
    } else {
        OUTPUT_SAVED_LIMIT
    };
    let spawned = spawn(
        &request.command,
        &request.cwd,
        save_limit,
        directory,
        env,
        agent,
    )
    .await?;
    let process = CommandProcess {
        output: Arc::clone(&spawned.process.output),
        end: spawned.process.end.clone(),
        control: Arc::clone(&spawned.process.control),
    };
    match call.run(process, request, cancel).await? {
        CommandRun::Background(output) => Ok(output),
        CommandRun::Ended {
            reference,
            end,
            stopped_by,
        } => {
            let mut output = ended_output(&spawned, &end, Some(json!(reference))).await;
            if stopped_by == Some(haarness_protocol::ShellCommandEndedBy::Person) {
                let how = match (&end.signal, end.exit_code) {
                    (Some(signal), _) => format!("it ended with {signal}"),
                    (None, Some(code)) => format!("it exited with code {code}"),
                    (None, None) => "it ended".to_owned(),
                };
                let _ = write!(
                    output.text,
                    "\n(The person stopped this command while it ran; {how}.)"
                );
                output.metadata["stoppedBy"] = json!("person");
            }
            Ok(output)
        }
    }
}

/// The result of a command that ended while its call waited: its exit code
/// and the end of its output, with all of it in a file when that is cut.
async fn ended_output(
    spawned: &Spawned,
    end: &ProcessEnd,
    command: Option<serde_json::Value>,
) -> ToolOutput {
    let output = &spawned.process.output;
    let exit = end.exit_code.unwrap_or(-1);
    let total = output.size();
    let saved = output.saved();
    let (stdout_tail, stdout_total, lines) = {
        let stats = spawned.stats.lock();
        (stats.stdout_tail.clone(), stats.stdout_total, stats.lines)
    };
    let truncated = total > SHOWN_BYTES as u64 || lines > SHOWN_LINES;
    // Cut by bytes, the end shown can start inside a character.
    let (tail, _) = output.tail(SHOWN_BYTES);
    let text = String::from_utf8_lossy(&tail);
    let preview = last_lines(&text, SHOWN_LINES);
    let stdout_kept = if stdout_total > stdout_tail.len() {
        haarness_core::whole_start(&stdout_tail)
    } else {
        &stdout_tail
    };
    let stdout_text = String::from_utf8_lossy(stdout_kept);
    let stdout_lines = stdout_text.lines().count();
    let mut metadata = json!({"exit":exit,"truncated":truncated,"stdout": if stdout_lines > SHOWN_LINES { last_lines(&stdout_text, SHOWN_LINES) } else { stdout_text.clone().into_owned() }});
    let agents = command.is_some();
    if let Some(command) = command {
        metadata["command"] = command;
    }
    // `stdout` holds only its end: a definition's snippet says so and names
    // the file, kept for it even when the output itself fits (2001 lines
    // with no final newline).
    let stdout_cut = stdout_total > stdout_tail.len() || stdout_lines > SHOWN_LINES;
    if stdout_cut {
        metadata["stdoutCut"] = json!(true);
    }
    let notice = if truncated || stdout_cut {
        metadata["outputPath"] = json!(output.path());
        if saved < total {
            metadata["outputSavedBytes"] = json!(saved);
        }
        if !truncated {
            String::new()
        } else if saved < total {
            format!(
                "First {} MiB of the output saved to: {}\n",
                OUTPUT_SAVED_LIMIT / (1024 * 1024),
                output.path().display()
            )
        } else {
            format!("Full output saved to: {}\n", output.path().display())
        }
    } else {
        output.discard_file().await;
        String::new()
    };
    let mut text = format!(
        "exit: {exit}\n{notice}{}",
        if truncated {
            preview.as_str()
        } else {
            text.as_ref()
        }
    );
    if end.leftovers {
        metadata["leftovers"] = json!(true);
        text.push_str(if end.leftovers_running {
            "\n(It left processes running that held its output open after it ended. Some were out of its reach and still run; their output is no longer read.)"
        } else {
            "\n(It left processes running that held its output open after it ended, started with & or in the background of the shell; they were stopped with it.)"
        });
        if end.leftovers_running {
            metadata["leftoversRunning"] = json!(true);
        }
        // Only the agent can run it otherwise.
        if agents {
            text.push_str(
                " For a server or another process that should keep running, use run_in_background.",
            );
        }
    }
    ToolOutput { text, metadata }
}

/// Remove the output files of commands that ran in the background in a
/// Haarness process that is gone (a crash, a shutdown that stopped them
/// with more output than memory kept): only `background-<pid>-…` files of
/// `folder`, whose process no longer runs. Those of this process, and of
/// one still running, stay. Unix only: elsewhere nothing is removed.
pub(crate) fn sweep_background_files(folder: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(rest) = name
            .to_str()
            .and_then(|name| name.strip_prefix(haarness_core::BACKGROUND_FILE_PREFIX))
        else {
            continue;
        };
        let Some(pid) = rest
            .split_once('-')
            .and_then(|(pid, _)| pid.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == std::process::id() || process_runs(pid) {
            continue;
        }
        if entry.file_type().is_ok_and(|kind| kind.is_file())
            && std::fs::remove_file(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

/// Whether process `pid` runs (or cannot be told not to).
fn process_runs(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return true;
        };
        !matches!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
            Err(nix::errno::Errno::ESRCH)
        )
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// The last `count` lines of `text`.
fn last_lines(text: &str, count: usize) -> String {
    text.lines()
        .rev()
        .take(count)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

/// What a command without a deadline saves of its output, from the start;
/// the end it shows is kept apart. 64 MiB, as much as a request body.
#[cfg(test)]
const SAVED_WITHOUT_DEADLINE: usize = 64 * 1024 * 1024;

/// The newlines in `bytes`; a plain count, not worth a dependency.
#[allow(clippy::naive_bytecount)]
fn newlines(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&byte| byte == b'\n').count()
}

/// Marks a command the agent runs as an agent's (`AGENT`, `AI_AGENT`, and
/// `HAARNESS`), for scripts and tools that adapt to one, and names the chat
/// it runs for (`HAARNESS_SESSION_ID`). They win over the server's own
/// environment, which a `haarness` started by another chat's command inherits
/// with that chat's id, and over `env` (`shell.env` hooks and extensions). An
/// `AI_AGENT` already set, there or in the server's environment, names an
/// outer agent and is kept, unless it is Haarness's own. A person's own
/// command is not marked.
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
    async fn stdout_says_when_only_its_end_was_kept() {
        let root = fixture();
        let stdout = |command: &'static str| {
            let root = root.clone();
            async move {
                run(
                    command,
                    &root,
                    0,
                    &CancellationToken::new(),
                    &root,
                    &BTreeMap::new(),
                    None,
                )
                .await
                .unwrap()
                .metadata
            }
        };
        // Within both limits: as it was, with no flag.
        let whole = stdout("seq 2000").await;
        assert_eq!(whole["stdout"].as_str().unwrap().lines().count(), 2000);
        assert!(whole.get("stdoutCut").is_none(), "{whole}");
        // Over the line limit: its last 2000 lines.
        let lines = stdout("seq 3000").await;
        let kept = lines["stdout"].as_str().unwrap();
        assert!(
            kept.starts_with("1001\n") && kept.ends_with("\n3000"),
            "{kept:.20}"
        );
        assert_eq!(lines["stdoutCut"], true);
        // 2001 lines with no final newline fit the output's limits, which
        // count newlines, yet stdout keeps 2000 of them: the file that holds
        // all of it stays for the note to name.
        let unended = stdout("seq 2000; printf 2001").await;
        assert_eq!(unended["stdoutCut"], true);
        assert_eq!(unended["truncated"], false);
        let saved = std::fs::read_to_string(unended["outputPath"].as_str().unwrap()).unwrap();
        assert!(saved.starts_with("1\n2\n") && saved.ends_with("\n2001"));
        // Over the byte limit on a line of two-byte characters, cut at an odd
        // byte: the kept text starts on a whole character.
        let bytes =
            stdout("i=0; while [ $i -lt 30001 ]; do printf 'é'; i=$((i+1)); done; printf x").await;
        let kept = bytes["stdout"].as_str().unwrap();
        assert_eq!(bytes["stdoutCut"], true);
        let head = kept.chars().take(4).collect::<String>();
        assert!(kept.starts_with('é') && kept.ends_with('x'), "{head:?}");
        assert!(!kept.contains('\u{fffd}'));
        // Errors alone past the limit leave stdout whole.
        let errors = stdout("seq 3000 >&2; printf done").await;
        assert_eq!(errors["stdout"], "done");
        assert!(errors.get("stdoutCut").is_none(), "{errors}");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn a_command_without_a_deadline_saves_only_the_start_of_its_output() {
        let root = fixture();
        let past = SAVED_WITHOUT_DEADLINE + 1024 * 1024;
        let output = run(
            &format!("yes 0123456789 | head -c {past}"),
            &root,
            0,
            &CancellationToken::new(),
            &root,
            &BTreeMap::new(),
            None,
        )
        .await
        .unwrap();
        let path = output.metadata["outputPath"].as_str().unwrap();
        assert_eq!(
            std::fs::metadata(path).unwrap().len(),
            SAVED_WITHOUT_DEADLINE as u64
        );
        assert_eq!(output.metadata["outputSavedBytes"], SAVED_WITHOUT_DEADLINE);
        assert!(
            output
                .text
                .contains(&format!("First 64 MiB of the output saved to: {path}")),
            "{:.200}",
            output.text
        );
        // Its end is still what the command shows.
        assert!(output.text.ends_with("0123456789"), "{:.200}", output.text);
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

    #[tokio::test]
    async fn a_character_split_between_reads_and_pipes_stays_whole() {
        let root = fixture();
        // The first byte of é, then a byte on stderr, then the second byte
        // of é: each its own read of its pipe.
        let output = run(
            r"printf '\303'; sleep .1; printf x >&2; sleep .1; printf '\251'",
            &root,
            0,
            &CancellationToken::new(),
            &root,
            &BTreeMap::new(),
            None,
        )
        .await
        .unwrap();
        assert!(!output.text.contains('\u{fffd}'), "{:?}", output.text);
        assert!(output.text.ends_with("xé"), "{:?}", output.text);
        assert_eq!(output.metadata["stdout"], "é");
        // The end shown when the output is past 50 KiB starts on a whole
        // character too: 30001 two-byte characters and one byte cut at an
        // odd byte.
        let output = run(
            "i=0; while [ $i -lt 30001 ]; do printf 'é'; i=$((i+1)); done; printf x",
            &root,
            0,
            &CancellationToken::new(),
            &root,
            &BTreeMap::new(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(output.metadata["truncated"], true);
        let shown = output.text.lines().last().unwrap();
        assert!(
            shown.starts_with('é') && shown.ends_with('x'),
            "{:?}",
            shown.chars().take(4).collect::<String>()
        );
        assert!(!output.text.contains('\u{fffd}'));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn leftovers_that_escape_the_command_are_said_to_run_on_and_a_person_gets_no_advice() {
        let root = fixture();
        let pid = root.join("escaped.pid");
        // A child in a session of its own (`ssh -f`, a daemon) that holds
        // the command's output open: out of reach of its process group.
        let escape = format!(
            "/usr/bin/python3 -c 'import os,time\nif os.fork()==0:\n    os.setsid(); open(\"{}\",\"w\").write(str(os.getpid())); time.sleep(20)'; echo parent-done",
            pid.display()
        );
        let output = run(
            &escape,
            &root,
            0,
            &CancellationToken::new(),
            &root,
            &BTreeMap::new(),
            None,
        )
        .await
        .unwrap();
        let escaped = std::fs::read_to_string(&pid).unwrap();
        let _ = std::process::Command::new("kill")
            .args(["-9", escaped.trim()])
            .status();
        assert_eq!(output.metadata["leftovers"], true);
        assert_eq!(output.metadata["leftoversRunning"], true, "{}", output.text);
        assert!(output.text.contains("still run"), "{}", output.text);
        assert!(!output.text.contains("were stopped"), "{}", output.text);
        // A person's own command gets no advice meant for the agent.
        assert!(
            !output.text.contains("run_in_background"),
            "{}",
            output.text
        );
        let output = run(
            "sleep 30 & echo started",
            &root,
            0,
            &CancellationToken::new(),
            &root,
            &BTreeMap::new(),
            None,
        )
        .await
        .unwrap();
        assert!(output.text.contains("were stopped"), "{}", output.text);
        assert!(
            !output.text.contains("run_in_background"),
            "{}",
            output.text
        );
        assert!(output.metadata.get("leftoversRunning").is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_process_that_keeps_printing_after_the_command_ended_is_stopped() {
        let root = fixture();
        // Its output never stops while the command runs on (`sleep 1`):
        // the command's end is still seen.
        for command in [
            "yes printing & echo started",
            "yes printing & sleep 1; echo started",
        ] {
            let began = std::time::Instant::now();
            let output = tokio::time::timeout(
                Duration::from_secs(15),
                run(
                    command,
                    &root,
                    0,
                    &CancellationToken::new(),
                    &root,
                    &BTreeMap::new(),
                    None,
                ),
            )
            .await
            .expect("the call returns")
            .unwrap();
            assert_eq!(output.metadata["leftovers"], true, "{command}");
            assert_eq!(output.metadata["exit"], 0, "{command}");
            // Its end, then two seconds at most for what it left (with room
            // for a loaded machine), not the outer limit.
            assert!(
                began.elapsed() < Duration::from_secs(10),
                "{command}: {:?}",
                began.elapsed()
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_the_files_of_background_commands_of_a_gone_process_are_swept() {
        let root = fixture();
        let mut gone = std::process::Command::new("true").spawn().unwrap();
        let gone_pid = gone.id();
        gone.wait().unwrap();
        let names = [
            format!("background-{gone_pid}-tool_a.txt"),
            format!("background-{}-tool_b.txt", std::process::id()),
            "tool_c.txt".to_owned(),
            "background-notapid-tool_d.txt".to_owned(),
        ];
        for name in &names {
            std::fs::write(root.join(name), "output").unwrap();
        }
        assert_eq!(sweep_background_files(&root), 1);
        for (index, name) in names.iter().enumerate() {
            assert_eq!(root.join(name).exists(), index != 0, "{name}");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_quiet_command_is_never_taken_for_one_that_left_processes_running() {
        let root = fixture();
        for attempt in 0..30 {
            for command in ["sleep 0.4", "sleep 0.4; echo done >&2"] {
                let output = run(
                    command,
                    &root,
                    0,
                    &CancellationToken::new(),
                    &root,
                    &BTreeMap::new(),
                    None,
                )
                .await
                .unwrap();
                assert!(
                    output.metadata.get("leftovers").is_none(),
                    "{command} (attempt {attempt}): {}",
                    output.text
                );
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_process_left_running_with_an_ampersand_does_not_hold_the_call() {
        let root = fixture();
        let started = std::time::Instant::now();
        let output = tokio::time::timeout(
            Duration::from_secs(15),
            run(
                "sleep 30 & echo $! > pid; echo started",
                &root,
                0,
                &CancellationToken::new(),
                &root,
                &BTreeMap::new(),
                None,
            ),
        )
        .await
        .expect("the call returns once the command has ended")
        .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(output.metadata["exit"], 0);
        assert_eq!(output.metadata["stdout"], "started\n");
        // What it left running was stopped with it, and the call says so.
        let pid = std::fs::read_to_string(root.join("pid")).unwrap();
        let alive = std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .status()
            .unwrap()
            .success();
        assert!(!alive, "sleep {pid} still runs");
        assert_eq!(output.metadata["leftovers"], true);
        assert!(output.text.contains("were stopped"), "{}", output.text);
        std::fs::remove_dir_all(root).unwrap();
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
