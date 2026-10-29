//! The runtime's side of the agents' shell commands: what a chat records and
//! is told as a command runs in the background (see
//! [`crate::background_commands`] for the commands themselves).

use std::{collections::HashMap, fmt::Write as _, sync::Arc};

use haarness_protocol::{
    InputDelivery, ItemData, QueueInputRequest, RequestId, ShellCommandEndedBy,
    ShellCommandMovedBy, ShellCommandOutputEvent, ShellCommandState, ThreadId,
};
use serde_json::json;

use super::{
    AgentRuntime, BACKGROUND_STORAGE_BACKOFF, BACKGROUND_STORAGE_RETRIES, CoreError, RunnerTask,
    refusal_reason,
};
use crate::background_commands::{
    CommandEntry, CommandHooks, ShellCall, ShellCallInner, StopSequence,
};

/// A chat's end of its commands: events, checkpoints, notices and hooks.
pub(super) struct CommandHost {
    /// The live runtime: its current configuration and hooks apply.
    runtime: AgentRuntime,
    hook_call: crate::HookCall,
    /// How the turn that started the command ran, for the turn its notice
    /// starts (as a background subagent's result does).
    auto_approve: bool,
    unattended: bool,
    tools: Option<HashMap<String, bool>>,
    follows_background: bool,
}

impl CommandHost {
    async fn checkpoint(&self, entry: &CommandEntry) {
        let info = entry.info();
        let data = ItemData::CommandState {
            call_id: info.call_id.clone(),
            command_id: info.id.clone(),
            state: info.state,
            exit_code: info.exit_code,
            signal: info.signal.clone(),
            tail: entry.checkpoint_tail(),
            started_at: info.started_at,
            ended_at: info.ended_at,
            moved_by: info.moved_by,
        };
        match self
            .runtime
            .repository
            .append_item(info.thread_id, Some(info.turn_id), data)
            .await
        {
            Ok(item) => {
                self.runtime.events.publish_scoped(
                    "item.created",
                    Some(info.thread_id),
                    Some(info.root_thread_id),
                    Some(info.turn_id),
                    Some(item.id),
                    json!(item),
                );
            }
            Err(error) => {
                tracing::error!(%error, command = %info.id, "failed to record a command checkpoint");
            }
        }
    }

    async fn notify_hooks(&self, kind: &str, message: &str, detail: serde_json::Value) {
        if let Some(hooks) = &self.runtime.components().hooks {
            hooks
                .command_notification(&self.hook_call, kind, message, &detail)
                .await;
        }
    }

    /// Tell the chat the command ended, through its input, as a background
    /// subagent's result is: steered into a running turn, or a turn of its
    /// own. It comes after the result of the call that started it.
    async fn deliver(&self, entry: &CommandEntry) {
        let info = entry.info();
        let thread = info.thread_id;
        // Owed from now on, so the chat is not quiet until the notice is in.
        let handoff = self
            .runtime
            .background_ledger
            .hand_off(thread, RequestId::new(), || {});
        entry.wait_released().await;
        let text = entry.notice();
        let mut attempt = 0_u32;
        let delivered = loop {
            let delivered = self
                .runtime
                .queue_input_with_options(
                    thread,
                    QueueInputRequest {
                        delivery: InputDelivery::Steer,
                        request_id: handoff.request_id(),
                        text: text.clone(),
                        synthetic: true,
                        follows_background: self.follows_background,
                        ..Default::default()
                    },
                    self.auto_approve,
                    self.unattended,
                    self.tools.clone(),
                    Some(&handoff),
                )
                .await;
            match delivered {
                Err(CoreError::Storage(_)) if attempt < BACKGROUND_STORAGE_RETRIES => {
                    attempt += 1;
                    tokio::time::sleep(BACKGROUND_STORAGE_BACKOFF * attempt).await;
                }
                delivered => break delivered,
            }
        };
        if let Err(error) = delivered {
            let notice = format!(
                "The end of the command \u{201c}{}\u{201d} could not start a turn in this chat ({}); your next message passes it to the agent.",
                short_line(&info.command),
                refusal_reason(&error)
            );
            self.runtime
                .keep_refused_background_result(thread, text, notice, &error)
                .await;
        }
        drop(handoff);
    }
}

/// The first line of a command, short enough for a notice.
fn short_line(command: &str) -> String {
    let line = command.lines().next().unwrap_or_default().trim();
    let mut short = line.chars().take(80).collect::<String>();
    if short.len() < line.len() || command.trim().lines().nth(1).is_some() {
        short.push('…');
    }
    short
}

#[async_trait::async_trait]
impl CommandHooks for CommandHost {
    fn updated(&self, info: &haarness_protocol::ShellCommandInfo) {
        self.runtime.events.publish_scoped(
            "command.updated",
            Some(info.thread_id),
            Some(info.root_thread_id),
            Some(info.turn_id),
            None,
            json!(info),
        );
    }

    fn printed(&self, info: &haarness_protocol::ShellCommandInfo, size: u64) {
        self.runtime.events.publish_scoped(
            "command.output",
            Some(info.thread_id),
            Some(info.root_thread_id),
            Some(info.turn_id),
            None,
            json!(ShellCommandOutputEvent {
                id: info.id.clone(),
                size,
            }),
        );
    }

    async fn backgrounded(&self, entry: &CommandEntry, moved: Option<ShellCommandMovedBy>) {
        self.checkpoint(entry).await;
        if let Some(by) = moved {
            let info = entry.info();
            let elapsed = info
                .background_at
                .map_or(0, |at| (at - info.started_at).num_milliseconds().max(0));
            let reason = match by {
                ShellCommandMovedBy::Person => "person",
                _ => "limit",
            };
            self.notify_hooks(
                "command_moved",
                "A command moved to the background",
                json!({"command_id": info.id, "reason": reason, "elapsed_ms": elapsed}),
            )
            .await;
        }
    }

    async fn ended(&self, entry: &CommandEntry, notify: bool) {
        let info = entry.info();
        // A shutdown records nothing: the next start finds the command still
        // running in the chat's record and says it was cut off. One that a
        // person or the agent had begun to stop records its end; nobody is
        // told of it.
        if entry.cut_off_by_shutdown() {
            return;
        }
        self.checkpoint(entry).await;
        if info.state == ShellCommandState::TimedOut {
            let elapsed = info
                .ended_at
                .map_or(0, |at| (at - info.started_at).num_milliseconds().max(0));
            self.notify_hooks(
                "command_time_limit",
                "A command in the background ran out of time",
                json!({"command_id": info.id, "reason": "time_limit", "elapsed_ms": elapsed}),
            )
            .await;
        }
        if notify {
            self.deliver(entry).await;
        }
    }
}

impl RunnerTask {
    /// The runtime as it is when it is used, not as it was when this turn
    /// began: a command's end may start a turn hours later.
    fn live_runtime(&self) -> AgentRuntime {
        let mut runtime = self.runtime_handle();
        runtime.components = Arc::clone(&self.live_components);
        runtime.reload_gate = Arc::clone(&self.live_reload_gate);
        runtime
    }

    /// The command a `bash` call `call_id` of this turn runs.
    pub(super) fn shell_call(&self, call_id: &str) -> ShellCall {
        let host = CommandHost {
            runtime: self.live_runtime(),
            hook_call: self.hook_call(None),
            auto_approve: self.request.auto_approve,
            unattended: self.unattended,
            tools: if self.request.auto_approve {
                self.request.tools.clone()
            } else {
                None
            },
            follows_background: self.request.follows_background,
        };
        ShellCall::new(ShellCallInner {
            commands: Arc::clone(&self.shell_commands),
            hooks: Arc::new(host),
            thread_id: self.thread.id,
            root_thread_id: self.root_thread_id,
            turn_id: self.turn.id,
            call_id: call_id.to_owned(),
            settings: self.config.execution.clone(),
            follows: self.request.follows_background && !self.unattended,
            subagent: self.thread.is_subagent,
        })
    }

    /// The result of call `call_id` (a `bash` call, or a batch holding
    /// some) is recorded: the commands it started in or moved to the
    /// background may now tell their end.
    pub(super) fn release_command_delivery(&self, call_id: &str) {
        let step = format!("{call_id}:");
        for entry in self.shell_commands.all(|entry| {
            let info = entry.info();
            info.thread_id == self.thread.id
                && (info.call_id == call_id || info.call_id.starts_with(&step))
        }) {
            entry.release();
        }
    }

    /// This turn is over: every command it started may tell its end, and a
    /// subagent's commands still running stop, untold.
    pub(super) async fn end_turn_commands(&self) {
        for entry in self
            .shell_commands
            .all(|entry| entry.info().turn_id == self.turn.id)
        {
            entry.release();
        }
        if !self.thread.is_subagent {
            return;
        }
        let grace = std::time::Duration::from_millis(self.config.execution.shell_stop_grace_ms);
        let running = self
            .shell_commands
            .live(|entry| entry.info().thread_id == self.thread.id);
        for entry in &running {
            entry.stop(
                ShellCommandEndedBy::TurnEnded,
                ShellCommandState::Stopped,
                StopSequence::Graceful(grace),
            );
        }
        for entry in running {
            entry
                .settled(grace * 2 + std::time::Duration::from_secs(2))
                .await;
        }
    }

    /// `bash_output` and `bash_stop`, which read and stop this chat's
    /// commands.
    pub(super) async fn command_tool(
        &self,
        name: &str,
        arguments: &serde_json::Value,
    ) -> crate::CoreResult<crate::ToolOutput> {
        let threads = std::collections::HashSet::from([self.thread.id]);
        if name == "bash_stop" {
            crate::background_commands::bash_stop(
                &self.shell_commands,
                &threads,
                arguments,
                &self.cancel,
            )
            .await
        } else {
            crate::background_commands::bash_output(
                &self.shell_commands,
                &threads,
                arguments,
                &self.cancel,
            )
            .await
        }
    }
}

impl AgentRuntime {
    /// Stop the live commands of `threads`, for `ended_by`; none of them
    /// tells its chat. Answers how many began to stop.
    pub(super) fn stop_commands_of(
        &self,
        threads: &std::collections::HashSet<ThreadId>,
        ended_by: ShellCommandEndedBy,
    ) -> usize {
        let grace = std::time::Duration::from_millis(
            self.components().config.execution.shell_stop_grace_ms,
        );
        self.shell_commands
            .live(|entry| threads.contains(&entry.info().thread_id))
            .iter()
            .filter(|entry| {
                entry.stop(
                    ended_by,
                    ShellCommandState::Stopped,
                    StopSequence::Graceful(grace),
                )
            })
            .count()
    }

    /// Stop every live command as the server shuts down.
    pub(super) fn stop_commands_for_shutdown(&self) {
        let grace = std::time::Duration::from_millis(
            self.components()
                .config
                .execution
                .shell_stop_grace_ms
                .min(1_000),
        );
        for entry in self.shell_commands.live(|_| true) {
            entry.stop(
                ShellCommandEndedBy::Shutdown,
                ShellCommandState::Stopped,
                StopSequence::Graceful(grace),
            );
        }
    }

    /// Stop the live commands running in `folder` or below it (a worktree
    /// about to be reset or removed), untold. Answers how many.
    pub async fn stop_commands_in(&self, folder: &std::path::Path) -> usize {
        let folder = dunce_like(folder);
        let running = self
            .shell_commands
            .live(|entry| dunce_like(entry.cwd()).starts_with(&folder));
        let grace = std::time::Duration::from_millis(
            self.components().config.execution.shell_stop_grace_ms,
        );
        for entry in &running {
            entry.stop(
                ShellCommandEndedBy::ChatStopped,
                ShellCommandState::Stopped,
                StopSequence::Graceful(grace),
            );
        }
        for entry in &running {
            entry
                .settled(grace * 2 + std::time::Duration::from_secs(2))
                .await;
        }
        running.len()
    }

    /// Stop the command `id` of `thread` or a chat under it, for a person.
    pub(super) fn stop_command_for_person(
        &self,
        entry: &Arc<CommandEntry>,
    ) -> haarness_protocol::ShellCommandInfo {
        let grace = std::time::Duration::from_millis(
            self.components().config.execution.shell_stop_grace_ms,
        );
        entry.stop(
            ShellCommandEndedBy::Person,
            ShellCommandState::Stopped,
            StopSequence::Graceful(grace),
        );
        entry.info()
    }
}

/// `path` with its links resolved where it exists, for comparing folders.
fn dunce_like(path: &std::path::Path) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// What a chat is told of a command in the background that a restart cut
/// off, and the note its person sees.
pub(crate) fn interrupted_report(command: &crate::StrandedCommand) -> Vec<ItemData> {
    let line = short_line(&command.command);
    let mut tag = format!(
        "<command id=\"{}\" state=\"interrupted\">",
        command.command_id
    );
    let _ = write!(
        tag,
        "\n<summary>`{line}` was cut off: Haarness stopped while it ran in the background, and it was not started again. Check what it left behind before you run it again.</summary>"
    );
    if !command.tail.is_empty() {
        let _ = write!(
            tag,
            "\n<command_output>\n{}\n</command_output>",
            command.tail
        );
    }
    tag.push_str("\n</command>");
    vec![
        ItemData::UserMessage {
            text: tag,
            attachments: Vec::new(),
            queued: false,
            synthetic: true,
            command_line: None,
        },
        ItemData::Error {
            message: format!(
                "Haarness restarted while the command \u{201c}{line}\u{201d} ran in the background. It was stopped and not started again; your next message tells the agent."
            ),
            retryable: false,
        },
    ]
}
