//! Shell commands the agent runs, which can keep running in the background.
//!
//! Every `bash` call of an agent is a command with an id. One the agent
//! starts with `run_in_background`, or one that moves there while it runs (a
//! person sends it, or it outlives `execution.shell_move_after_ms` in a chat
//! whose client follows background work), keeps running after its call has
//! returned; when it ends, the chat is told in a hidden message. Clients list,
//! follow and stop commands through `/v1/threads/{id}/commands`, and follow
//! them live through `command.updated` and `command.output` events.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{ThreadId, TurnId};

/// Server capability for commands in the background: `run_in_background`,
/// the `bash_output` and `bash_stop` tools, the `/commands` routes, the
/// `command.*` events, `followsBackground` and
/// [`crate::ThreadActivity::background_commands`].
pub const BACKGROUND_COMMANDS_CAPABILITY: &str = "background-commands.v1";

/// Where a command is in its life. `running` and `stopping` are live; the
/// others are ends. Clients read `exited` with `exitCode` 0 as completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShellCommandState {
    Running,
    /// Asked to stop; it is still shutting down.
    Stopping,
    /// It ended on its own, with `exitCode` (or killed by a signal it was
    /// not sent by Haarness, with `signal`).
    Exited,
    /// A person, the agent, the end of its subagent's turn or of its chat,
    /// or Haarness shutting down stopped it (`endedBy` says which).
    Stopped,
    /// It ran past its time limit and was stopped.
    TimedOut,
    /// Haarness stopped or crashed while it ran: it was found still running
    /// in the chat's record after a restart.
    Interrupted,
    /// A state a newer server reports that this client does not know.
    #[serde(other)]
    Unknown,
}

impl ShellCommandState {
    /// The command may still print and change state.
    #[must_use]
    pub fn is_live(self) -> bool {
        matches!(self, Self::Running | Self::Stopping)
    }
}

/// How a command came to run in the background.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShellCommandMovedBy {
    /// The agent started it there (`run_in_background`).
    Agent,
    /// A person sent it there while it ran.
    Person,
    /// It ran longer than `execution.shell_move_after_ms`.
    Limit,
    /// A reason a newer server reports that this client does not know.
    #[serde(other)]
    Unknown,
}

/// What ended a command that did not end on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShellCommandEndedBy {
    /// A person stopped it (its row, `/stop`).
    Person,
    /// The agent stopped it (`bash_stop`).
    Agent,
    /// Its time limit ran out.
    TimeLimit,
    /// The turn it belonged to ended: a foreground command when its reply
    /// was stopped, a subagent's background command when the subagent's
    /// turn finished.
    TurnEnded,
    /// Its chat was stopped with nothing else running, archived or deleted.
    ChatStopped,
    /// Haarness shut down.
    Shutdown,
    /// A reason a newer server reports that this client does not know.
    #[serde(other)]
    Unknown,
}

/// One shell command an agent ran (`GET /v1/threads/{id}/commands`, the
/// `command.updated` event).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShellCommandInfo {
    /// The command's id, as the agent names it to `bash_output` and
    /// `bash_stop`, unique on this server.
    pub id: String,
    /// The chat whose agent ran it (a subagent's chat for its commands).
    pub thread_id: ThreadId,
    /// The topmost chat of that chat's tree.
    pub root_thread_id: ThreadId,
    /// The turn whose `bash` call started it.
    pub turn_id: TurnId,
    /// That call, by the id of its `tool_call` item.
    pub call_id: String,
    /// The command line as it runs (after any hook rewrote it), cut to its
    /// first 8 KiB.
    pub command: String,
    /// The folder it runs in, when the call named one; otherwise it runs in
    /// the chat's folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    /// It runs, or ran, in the background: its call returned before it
    /// ended, and its end is told to the chat.
    pub background: bool,
    /// How it came to the background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moved_by: Option<ShellCommandMovedBy>,
    pub state: ShellCommandState,
    /// Its exit code, once it exited with one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// The signal that ended it (`SIGTERM`), when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    /// What stopped it, when it did not end on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_by: Option<ShellCommandEndedBy>,
    pub started_at: DateTime<Utc>,
    /// When it went to the background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    /// When its time limit stops it, while it runs with one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stops_at: Option<DateTime<Utc>>,
    /// When it moves to the background by itself, while it runs in the
    /// foreground of a chat whose client follows background work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moves_at: Option<DateTime<Utc>>,
    /// How much it has printed, stdout and stderr together, in bytes.
    pub output_bytes: u64,
    /// Its end will be told to its chat in a hidden message (a background
    /// command of a root chat). A subagent's commands, and commands still in
    /// the foreground, deliver nothing.
    pub delivers_notice: bool,
}

/// `GET /v1/threads/{id}/commands/{command}/output`: a page of what the
/// command printed, stdout and stderr together in the order they came.
/// Offsets are bytes from the start of the whole output; a page is cut on
/// character boundaries, so a character split between two reads of the
/// command's pipes is never broken. The text is as printed, terminal escapes
/// included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShellCommandOutput {
    pub text: String,
    /// Where `text` starts. More than the `cursor` asked for when the output
    /// before it is no longer kept (a long-running command keeps its first
    /// 64 MiB and its last part).
    pub start: u64,
    /// Where `text` ends: the `cursor` of the next read.
    pub cursor: u64,
    /// How much the command has printed so far.
    pub size: u64,
    /// Output past `cursor` is already there to read.
    pub more: bool,
    /// The command has ended and all its output is in: `size` is final.
    pub settled: bool,
    pub state: ShellCommandState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

/// The `command.output` event: a command printed more, up to `size` bytes in
/// all. At most one a second per command; read the text from `/output`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShellCommandOutputEvent {
    pub id: String,
    pub size: u64,
}

/// `metadata.command` of the result of every agent `bash` call: the command
/// behind it, which a client follows by `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShellCommandRef {
    pub id: String,
    /// The call returned while the command ran on in the background.
    pub background: bool,
    pub started_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unknown_states_and_reasons_from_a_newer_server_still_read() {
        let state: ShellCommandState = serde_json::from_value(json!("paused")).unwrap();
        assert_eq!(state, ShellCommandState::Unknown);
        let moved: ShellCommandMovedBy = serde_json::from_value(json!("schedule")).unwrap();
        assert_eq!(moved, ShellCommandMovedBy::Unknown);
        let ended: ShellCommandEndedBy = serde_json::from_value(json!("budget")).unwrap();
        assert_eq!(ended, ShellCommandEndedBy::Unknown);
        assert_eq!(
            serde_json::to_value(ShellCommandState::TimedOut).unwrap(),
            "timed_out"
        );
        assert_eq!(
            serde_json::to_value(ShellCommandEndedBy::ChatStopped).unwrap(),
            "chat_stopped"
        );
    }

    #[test]
    fn a_command_leaves_out_what_it_does_not_have() {
        let info = ShellCommandInfo {
            id: "c1".into(),
            thread_id: ThreadId::new(),
            root_thread_id: ThreadId::new(),
            turn_id: TurnId::new(),
            call_id: "call".into(),
            command: "npm run dev".into(),
            workdir: None,
            background: true,
            moved_by: Some(ShellCommandMovedBy::Agent),
            state: ShellCommandState::Running,
            exit_code: None,
            signal: None,
            ended_by: None,
            started_at: Utc::now(),
            background_at: Some(Utc::now()),
            ended_at: None,
            stops_at: None,
            moves_at: None,
            output_bytes: 12,
            delivers_notice: true,
        };
        let value = serde_json::to_value(&info).unwrap();
        let keys = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            keys,
            [
                "background",
                "backgroundAt",
                "callId",
                "command",
                "deliversNotice",
                "id",
                "movedBy",
                "outputBytes",
                "rootThreadId",
                "startedAt",
                "state",
                "threadId",
                "turnId",
            ]
            .into_iter()
            .collect()
        );
        assert_eq!(
            serde_json::from_value::<ShellCommandInfo>(value).unwrap(),
            info
        );
    }
}
