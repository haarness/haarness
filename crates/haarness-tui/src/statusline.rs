use std::{process::Stdio, time::Duration};

use serde::Serialize;
use tokio::{
    io::AsyncWriteExt as _,
    process::Command,
    sync::{mpsc, watch},
};

use crate::{
    config::{StatusLineCommand, StatusLineConfig},
    state::App,
};

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    thread_id: String,
    title: String,
    agent: String,
    provider: String,
    model: String,
    workspace: Option<String>,
    git_branch: Option<String>,
    phase: Option<String>,
    active: bool,
}

impl Context {
    pub fn from_app(app: &App) -> Self {
        Self {
            thread_id: app.thread.id.to_string(),
            title: app.thread.title.clone(),
            agent: app.agent().to_owned(),
            provider: app.thread.provider.clone(),
            model: app.thread.model.clone(),
            workspace: app
                .server_info
                .as_ref()
                .and_then(|info| info.workspace.clone()),
            git_branch: app
                .server_info
                .as_ref()
                .and_then(|info| info.git_branch.clone()),
            phase: app.phase.as_ref().map(|phase| phase.state.clone()),
            active: app.active_turn.is_some(),
        }
    }
}

pub enum Update {
    Output(String),
    Error(String),
}

/// How loudly to log a status-line failure. The command runs every refresh
/// (2 s by default), so a broken one would otherwise warn forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Warn,
    Debug,
}

/// Remembers the last status-line failure so only a change is a warning.
#[derive(Debug, Default)]
pub struct FailureLog {
    last: Option<String>,
}

impl FailureLog {
    /// A failure: WARN when it differs from the last one, DEBUG while it repeats.
    pub fn failed(&mut self, error: &str) -> LogLevel {
        if self.last.as_deref() == Some(error) {
            return LogLevel::Debug;
        }
        self.last = Some(error.to_owned());
        LogLevel::Warn
    }

    /// A success: INFO with the cleared error when it ends a run of failures.
    pub fn recovered(&mut self) -> Option<String> {
        self.last.take()
    }
}

pub fn spawn(
    config: Option<StatusLineConfig>,
) -> (
    watch::Sender<Context>,
    mpsc::Receiver<Update>,
    tokio::task::JoinHandle<()>,
) {
    let (context_tx, context_rx) = watch::channel(Context::default());
    let (update_tx, update_rx) = mpsc::channel(2);
    let task = tokio::spawn(async move {
        let Some(config) = config else { return };
        run(config, context_rx, update_tx).await;
    });
    (context_tx, update_rx, task)
}

async fn run(
    config: StatusLineConfig,
    context: watch::Receiver<Context>,
    updates: mpsc::Sender<Update>,
) {
    let mut interval = tokio::time::interval(Duration::from_millis(config.refresh_ms));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let snapshot = context.borrow().clone();
        let update = match invoke(&config, &snapshot).await {
            Ok(output) => Update::Output(output),
            Err(error) => Update::Error(error),
        };
        if updates.send(update).await.is_err() {
            return;
        }
    }
}

async fn invoke(config: &StatusLineConfig, context: &Context) -> Result<String, String> {
    let input = serde_json::to_vec(context).map_err(|error| error.to_string())?;
    let mut command = match &config.command {
        // cmd.exe reads its own command line: hand it the source as written.
        #[cfg(windows)]
        StatusLineCommand::Shell(source) => {
            let mut command = Command::new("cmd");
            command.raw_arg(format!("/d /s /c \"{source}\""));
            command
        }
        #[cfg(not(windows))]
        StatusLineCommand::Shell(source) => {
            let mut command = Command::new("sh");
            command.args(["-lc", source]);
            command
        }
        StatusLineCommand::Argv(argv) => {
            let mut command = Command::new(&argv[0]);
            command.args(&argv[1..]);
            command
        }
    };
    command
        .env(
            "HAARNESS_STATUS_CONTEXT",
            String::from_utf8_lossy(&input).as_ref(),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&input)
            .await
            .map_err(|error| error.to_string())?;
    }
    let output = tokio::time::timeout(
        Duration::from_millis(config.timeout_ms),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| format!("timed out after {}ms", config.timeout_ms))?
    .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "exited {}: {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8(output.stdout).map_err(|_| "output was not UTF-8".to_owned())?;
    // The first line with something to read once its escape sequences go;
    // a row of the band, so its tabs and carriage returns go as well.
    let line = stdout
        .lines()
        .map(|line| {
            crate::escapes::without_escapes(line)
                .chars()
                .filter(|character| !character.is_control())
                .collect::<String>()
        })
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    Ok(line.chars().take(512).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_failures_warn_once_and_recovery_is_noted() {
        let mut log = FailureLog::default();
        assert_eq!(log.recovered(), None, "a healthy command logs nothing");
        assert_eq!(log.failed("exited 1: boom"), LogLevel::Warn);
        assert_eq!(log.failed("exited 1: boom"), LogLevel::Debug);
        assert_eq!(log.failed("exited 1: boom"), LogLevel::Debug);
        assert_eq!(log.failed("timed out after 5000ms"), LogLevel::Warn);
        assert_eq!(log.failed("timed out after 5000ms"), LogLevel::Debug);
        assert_eq!(log.recovered().as_deref(), Some("timed out after 5000ms"));
        assert_eq!(log.recovered(), None, "recovery is noted once");
        assert_eq!(
            log.failed("timed out after 5000ms"),
            LogLevel::Warn,
            "a failure after recovery warns again"
        );
    }

    /// A POSIX shell script as a status line command: in `sh`, which Git
    /// brings on Windows, where a shell command goes to cmd.exe.
    fn posix(script: &str) -> StatusLineCommand {
        if cfg!(windows) {
            StatusLineCommand::Argv(vec!["sh".into(), "-c".into(), script.into()])
        } else {
            StatusLineCommand::Shell(script.into())
        }
    }

    #[tokio::test]
    async fn command_receives_context_and_strips_ansi() {
        let config = StatusLineConfig {
            command: posix(
                "read payload; printf '\\033]0;title\\007\\033[31mcontext:%s\\033[0m\\nignored\\n' \"$payload\"",
            ),
            refresh_ms: 250,
            timeout_ms: 2_000,
        };
        let output = invoke(
            &config,
            &Context {
                title: "demo".into(),
                ..Context::default()
            },
        )
        .await
        .unwrap();
        // A window title goes whole, not only its escape byte.
        assert!(output.starts_with("context:{"), "{output}");
        assert!(output.contains("\"title\":\"demo\""));
        assert!(!output.contains('\u{1b}'));
        assert!(!output.contains("ignored"));
    }

    /// A first line that only sets the window title and the colours has
    /// nothing to read, so the band shows the line after it.
    #[tokio::test]
    async fn a_line_of_only_escape_sequences_gives_way_to_the_next() {
        let config = StatusLineConfig {
            command: posix(
                "cat >/dev/null; printf '\\033]2;branch\\007\\033[1m\\nmain \\342\\234\\223\\n'",
            ),
            refresh_ms: 250,
            timeout_ms: 2_000,
        };
        let output = invoke(&config, &Context::default()).await.unwrap();
        assert_eq!(output, "main ✓");
    }
}
