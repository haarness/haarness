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
        StatusLineCommand::Shell(source) if cfg!(windows) => {
            let mut command = Command::new("cmd");
            command.args(["/C", source]);
            command
        }
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
    let line = stdout
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(strip_controls)
        .unwrap_or_default();
    Ok(line.chars().take(512).collect())
}

fn strip_controls(value: &str) -> String {
    let mut output = String::new();
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        } else if !character.is_control() {
            output.push(character);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn command_receives_context_and_strips_ansi() {
        let config = StatusLineConfig {
            command: StatusLineCommand::Shell(
                "read payload; printf '\\033[31mcontext:%s\\033[0m\\nignored\\n' \"$payload\""
                    .into(),
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
        assert!(output.starts_with("context:{"));
        assert!(output.contains("\"title\":\"demo\""));
        assert!(!output.contains('\u{1b}'));
        assert!(!output.contains("ignored"));
    }
}
