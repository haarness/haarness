//! The agents' shell commands as the terminal UI follows them: which run in
//! the background and how each ended, sending running work there, and the
//! full-screen view of one command's output.
//!
//! The server owns every command (`background-commands.v1`). This client
//! keeps what its list and `command.updated` events say: every command of
//! the open conversation, ended ones included while the server keeps them,
//! and the running ones of other chats, for the session list. A row of the
//! transcript reads its command from there, else from the chat's durable
//! `command_state` items, which outlive the server's ten minutes and its
//! restarts.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{DateTime, Utc};
use haarness_protocol::{
    EventEnvelope, Item, ItemData, ShellCommandInfo, ShellCommandOutput, ShellCommandOutputEvent,
    ShellCommandRef, ShellCommandState, ThreadId,
};
use unicode_width::UnicodeWidthChar as _;

use crate::api::ApiClient;
use crate::locale::Language;
use crate::state::{App, OptionEntry, Overlay};

/// The rebindable action that sends running work to the background.
pub(crate) const BACKGROUND_ACTION: &str = "background";

/// A foreground command that has run this long names the key that sends it
/// to the background: a quick one never flashes a hint.
const MOVE_HINT_AFTER: chrono::Duration = chrono::Duration::seconds(3);

/// How long the server keeps an ended command in its list.
const ENDED_KEPT: chrono::Duration = chrono::Duration::minutes(10);

/// One read of a command's output, at most.
const OUTPUT_PAGE: u64 = 256 * 1024;

/// The most of a command's output the view keeps; older lines go first.
const OUTPUT_KEPT: usize = 2 * 1024 * 1024;

/// Output not all in yet is read again this often when no event has said
/// it grew (a missed event, the last bytes of a command that just ended).
const REREAD: Duration = Duration::from_secs(1);

/// Value prefix of a command's row in `/tasks`.
pub(crate) const TASKS_COMMAND: &str = "command:";

/// What this client knows of the agents' shell commands, by id.
#[derive(Default)]
pub(crate) struct ShellCommands {
    by_id: HashMap<String, ShellCommandInfo>,
    /// The conversation whose whole list came: a command its checkpoints
    /// call running that the list leaves out (one a forked chat copied, one
    /// a restart lost) is not running there.
    listed: Option<ThreadId>,
    /// Commands the server said it does not have (404 or 410 on a route).
    gone: HashSet<String>,
}

/// Whether `info` belongs to the conversation rooted at `conversation`.
fn in_tree(info: &ShellCommandInfo, conversation: ThreadId) -> bool {
    info.root_thread_id == conversation || info.thread_id == conversation
}

impl ShellCommands {
    pub(crate) fn get(&self, id: &str) -> Option<&ShellCommandInfo> {
        self.by_id.get(id)
    }

    /// The command a `bash` call of `thread` started, by its call (and turn,
    /// since a provider may reuse a call id in a later turn).
    pub(crate) fn for_call(
        &self,
        thread: ThreadId,
        turn: Option<haarness_protocol::TurnId>,
        call_id: &str,
    ) -> Option<&ShellCommandInfo> {
        self.by_id.values().find(|info| {
            info.thread_id == thread
                && info.call_id == call_id
                && turn.is_none_or(|turn| info.turn_id == turn)
        })
    }

    /// Take a command the server reported. Another conversation's command
    /// is kept only while it runs.
    pub(crate) fn update(&mut self, info: ShellCommandInfo, conversation: ThreadId) {
        if info.state.is_live() || in_tree(&info, conversation) {
            self.by_id.insert(info.id.clone(), info);
        } else {
            self.by_id.remove(&info.id);
        }
    }

    /// The server's whole list for `conversation`, and what is known of the
    /// running commands of every chat (`active`, when it was read).
    pub(crate) fn hydrate(
        &mut self,
        conversation: ThreadId,
        tree: Option<Vec<ShellCommandInfo>>,
        active: Option<Vec<ShellCommandInfo>>,
    ) {
        if let Some(tree) = tree {
            self.by_id.retain(|_, info| !in_tree(info, conversation));
            for info in tree {
                self.by_id.insert(info.id.clone(), info);
            }
            self.listed = Some(conversation);
        } else if self.listed != Some(conversation) {
            self.listed = None;
        }
        if let Some(active) = active {
            // Another chat's command that is not running any more ended while
            // nothing said so.
            self.by_id
                .retain(|_, info| in_tree(info, conversation) || !info.state.is_live());
            for info in active {
                if !in_tree(&info, conversation) || !self.by_id.contains_key(&info.id) {
                    self.by_id.insert(info.id.clone(), info);
                }
            }
        }
        self.by_id
            .retain(|_, info| info.state.is_live() || in_tree(info, conversation));
    }

    /// Commands of the conversation: running ones first, oldest first, then
    /// the ended ones the server still keeps, the latest first.
    pub(crate) fn of(&self, conversation: ThreadId) -> Vec<&ShellCommandInfo> {
        let now = Utc::now();
        let mut commands: Vec<_> = self
            .by_id
            .values()
            .filter(|info| in_tree(info, conversation))
            .filter(|info| {
                info.state.is_live() || info.ended_at.is_none_or(|ended| now - ended < ENDED_KEPT)
            })
            .collect();
        commands.sort_by(|left, right| {
            right
                .state
                .is_live()
                .cmp(&left.state.is_live())
                .then_with(|| {
                    if left.state.is_live() {
                        left.started_at.cmp(&right.started_at)
                    } else {
                        right.ended_at.cmp(&left.ended_at)
                    }
                })
                .then_with(|| left.id.cmp(&right.id))
        });
        commands
    }

    /// Commands running in the background in the conversation rooted at
    /// `conversation`.
    pub(crate) fn running_in_background(&self, conversation: ThreadId) -> usize {
        self.by_id
            .values()
            .filter(|info| info.background && info.state.is_live() && in_tree(info, conversation))
            .count()
    }

    /// Whether a command this client has no report of may still run: not
    /// once the conversation's whole list came without it, nor once the
    /// server said it does not have it.
    fn may_run(&self, id: &str, conversation: ThreadId) -> bool {
        self.listed != Some(conversation) && !self.gone.contains(id)
    }

    /// The server does not have the command (any more).
    pub(crate) fn forget(&mut self, id: &str) {
        self.by_id.remove(id);
        self.gone.insert(id.to_owned());
    }

    fn set_output_size(&mut self, id: &str, size: u64) {
        if let Some(info) = self.by_id.get_mut(id) {
            info.output_bytes = info.output_bytes.max(size);
        }
    }
}

/// The last `command_state` item of each command in a transcript, by its
/// id and by the call (and turn) that started it.
pub(crate) struct DurableStates<'a> {
    by_command: HashMap<&'a str, &'a ItemData>,
    by_call: HashMap<(&'a str, Option<haarness_protocol::TurnId>), &'a ItemData>,
}

pub(crate) fn durable_states(items: &[Item]) -> DurableStates<'_> {
    let mut by_command = HashMap::new();
    let mut by_call = HashMap::new();
    for item in items {
        if let ItemData::CommandState {
            command_id,
            call_id,
            ..
        } = &item.data
        {
            by_command.insert(command_id.as_str(), &item.data);
            by_call.insert((call_id.as_str(), item.turn_id), &item.data);
        }
    }
    DurableStates {
        by_command,
        by_call,
    }
}

impl DurableStates<'_> {
    fn tail(&self, id: &str) -> Option<&str> {
        match self.by_command.get(id) {
            Some(ItemData::CommandState { tail, .. }) => Some(tail),
            _ => None,
        }
    }
}

/// The command behind a `bash` row, as far as this client knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandRow {
    pub id: String,
    /// The chat whose agent ran it.
    pub thread: ThreadId,
    /// It runs, or ran, in the background.
    pub background: bool,
    pub state: ShellCommandState,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
}

impl CommandRow {
    pub(crate) fn of_info(info: &ShellCommandInfo) -> Self {
        Self {
            id: info.id.clone(),
            thread: info.thread_id,
            background: info.background,
            state: info.state,
            exit_code: info.exit_code,
            signal: info.signal.clone(),
            started_at: Some(info.started_at),
        }
    }

    /// The task state whose marker and colour the row takes: `background`
    /// spins, `done`, `failed` and `cancelled` say how it ended.
    pub(crate) fn task_state(&self) -> &'static str {
        task_state(self.state, self.exit_code, self.signal.as_deref())
    }

    /// How it ended, in the row's words; `None` while it runs.
    pub(crate) fn ending(&self, language: Language) -> Option<String> {
        ending(language, self.state, self.exit_code, self.signal.as_deref())
    }

    /// How long it has run, while it runs.
    pub(crate) fn elapsed(&self) -> Option<String> {
        self.started_at
            .filter(|_| self.state == ShellCommandState::Running)
            .map(elapsed_since)
    }

    /// The row's state words: `in background · 1m 12s`, then how it ended.
    /// `compact` leaves out what a narrow row can do without.
    pub(crate) fn status(&self, language: Language, compact: bool) -> String {
        let lead = language.text("in background");
        match (self.ending(language), compact) {
            (Some(ending), false) => format!("{lead} · {ending}"),
            (Some(ending), true) => ending,
            (None, false) => self
                .elapsed()
                .map_or_else(|| lead.to_owned(), |elapsed| format!("{lead} · {elapsed}")),
            (None, true) => lead.to_owned(),
        }
    }
}

fn task_state(
    state: ShellCommandState,
    exit_code: Option<i32>,
    signal: Option<&str>,
) -> &'static str {
    match state {
        ShellCommandState::Running | ShellCommandState::Stopping => "background",
        ShellCommandState::Exited if exit_code.unwrap_or(0) == 0 && signal.is_none() => "done",
        ShellCommandState::Exited | ShellCommandState::TimedOut => "failed",
        ShellCommandState::Stopped | ShellCommandState::Interrupted => "cancelled",
        ShellCommandState::Unknown => "unknown",
    }
}

fn ending(
    language: Language,
    state: ShellCommandState,
    exit_code: Option<i32>,
    signal: Option<&str>,
) -> Option<String> {
    Some(match state {
        ShellCommandState::Running | ShellCommandState::Unknown => return None,
        ShellCommandState::Stopping => language.text("stopping").to_owned(),
        ShellCommandState::Exited => match (exit_code, signal) {
            (Some(code), _) if code != 0 => {
                language.format("exit {code}", &[("code", &code.to_string())])
            }
            (None, Some(signal)) => signal.to_owned(),
            _ => language.text("completed").to_owned(),
        },
        ShellCommandState::Stopped => language.text("stopped").to_owned(),
        ShellCommandState::TimedOut => language.text("ran out of time").to_owned(),
        ShellCommandState::Interrupted => language.text("cut off by a restart").to_owned(),
    })
}

/// `1m 12s` since `started`.
fn elapsed_since(started: DateTime<Utc>) -> String {
    let seconds = (Utc::now() - started).num_seconds().max(0);
    crate::view::sidebar::format_goal_time(seconds.unsigned_abs())
}

/// The command a `bash` result names (`metadata.command`), read in place:
/// every row asks on every frame.
fn result_command(result: Option<&Item>) -> Option<ShellCommandRef> {
    let ItemData::ToolResult { metadata, .. } = &result?.data else {
        return None;
    };
    named_command(metadata)
}

/// `command` of a `bash` call's metadata.
fn named_command(metadata: &serde_json::Value) -> Option<ShellCommandRef> {
    let command = metadata.get("command")?;
    Some(ShellCommandRef {
        id: command.get("id")?.as_str()?.to_owned(),
        background: command
            .get("background")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        started_at: command
            .get("startedAt")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| value.parse().ok())?,
    })
}

/// The command behind the `bash` call `call` (with its result, once it
/// came): the server's report while it keeps one, else the chat's last
/// `command_state` item for it, else what the result says. `None` for a call
/// this client knows no command of (an older server, a quick command the
/// server never reported).
pub(crate) fn command_row(
    app: &App,
    durable: &DurableStates<'_>,
    call: &Item,
    call_id: &str,
    result: Option<&Item>,
) -> Option<CommandRow> {
    row_for(app, durable, call, call_id, result_command(result))
}

/// The command behind the `bash` step `step` (counted from 1) of the batch
/// call `call`, which runs as call `<call id>:<step>`; the batch's result,
/// once it came, names it in that step's entry.
pub(crate) fn step_command_row(
    app: &App,
    durable: &DurableStates<'_>,
    call: &Item,
    call_id: &str,
    step: usize,
    result: Option<&Item>,
) -> Option<CommandRow> {
    let named = result.and_then(|result| match &result.data {
        ItemData::ToolResult { metadata, .. } => metadata
            .get("results")
            .and_then(|results| results.get(step.checked_sub(1)?))
            .and_then(|entry| named_command(entry.get("metadata")?)),
        _ => None,
    });
    row_for(app, durable, call, &format!("{call_id}:{step}"), named)
}

fn row_for(
    app: &App,
    durable: &DurableStates<'_>,
    call: &Item,
    call_id: &str,
    named: Option<ShellCommandRef>,
) -> Option<CommandRow> {
    let info = named
        .as_ref()
        .and_then(|named| app.shell_commands.get(&named.id))
        .or_else(|| {
            app.shell_commands
                .for_call(call.thread_id, call.turn_id, call_id)
        });
    if let Some(info) = info {
        let mut row = CommandRow::of_info(info);
        // A command that moved there is in the background even when the
        // report of it is older than the call's answer.
        row.background |= named.as_ref().is_some_and(|named| named.background);
        return Some(row);
    }
    let checkpoint = named
        .as_ref()
        .and_then(|named| durable.by_command.get(named.id.as_str()))
        .or_else(|| durable.by_call.get(&(call_id, call.turn_id)));
    let (id, mut row) = if let Some(ItemData::CommandState {
        command_id,
        state,
        exit_code,
        signal,
        started_at,
        ..
    }) = checkpoint
    {
        (
            command_id.clone(),
            CommandRow {
                id: command_id.clone(),
                thread: call.thread_id,
                background: true,
                state: *state,
                exit_code: *exit_code,
                signal: signal.clone(),
                started_at: Some(*started_at),
            },
        )
    } else {
        // Just sent to the background, before its report or checkpoint came.
        let named = named.filter(|named| named.background)?;
        (
            named.id.clone(),
            CommandRow {
                id: named.id,
                thread: call.thread_id,
                background: true,
                state: ShellCommandState::Running,
                exit_code: None,
                signal: None,
                started_at: Some(named.started_at),
            },
        )
    };
    // Running, says the chat's record; but the server's list of the chat came
    // without it (a forked chat's copy, a restart), or the server said it has
    // no such command: it runs nowhere this client can follow.
    if row.state.is_live() && !app.shell_commands.may_run(&id, app.conversation_id()) {
        row.state = ShellCommandState::Unknown;
    }
    Some(row)
}

impl App {
    /// The server keeps commands in the background (`background-commands.v1`).
    pub(crate) fn background_commands_supported(&self) -> bool {
        self.server_info.as_ref().is_some_and(|info| {
            info.capabilities
                .iter()
                .any(|capability| capability == haarness_protocol::BACKGROUND_COMMANDS_CAPABILITY)
        })
    }

    /// The conversation on screen: the chat itself, or the one a child
    /// agent's view was opened from.
    pub(crate) fn conversation_id(&self) -> ThreadId {
        self.parent_stack
            .first()
            .map_or(self.thread.id, |thread| thread.id)
    }

    /// Commands of the open conversation running in the background.
    pub(crate) fn background_command_count(&self) -> usize {
        self.shell_commands
            .running_in_background(self.conversation_id())
    }

    /// The commands the sidebar's `background` section lists.
    pub(crate) fn background_commands(&self) -> Vec<&ShellCommandInfo> {
        self.shell_commands
            .of(self.conversation_id())
            .into_iter()
            .filter(|info| info.background)
            .collect()
    }

    /// Read the open conversation's commands, and the running ones of every
    /// chat, from a server that keeps them.
    pub(crate) async fn hydrate_shell_commands(&mut self, client: &ApiClient) {
        if !self.background_commands_supported() {
            self.shell_commands = ShellCommands::default();
            return;
        }
        let conversation = self.conversation_id();
        // Both reads at once: opening a chat waits for the slower only.
        let (tree, active) = tokio::join!(
            client.shell_commands(conversation),
            client.active_shell_commands()
        );
        self.shell_commands
            .hydrate(conversation, tree.ok(), active.ok());
    }

    /// Follow `command.updated` and `command.output` from every chat: the
    /// session list counts what runs in the background anywhere.
    pub(crate) fn follow_shell_command(&mut self, event: &EventEnvelope) -> bool {
        match event.event_type.as_str() {
            "command.updated" => {
                if let Ok(info) = serde_json::from_value::<ShellCommandInfo>(event.payload.clone())
                {
                    if let Some(view) = self.command_view.as_mut()
                        && view.id == info.id
                    {
                        view.wanted |= info.output_bytes > view.cursor || !info.state.is_live();
                    }
                    let conversation = self.conversation_id();
                    self.shell_commands.update(info, conversation);
                }
                true
            }
            "command.output" => {
                if let Ok(output) =
                    serde_json::from_value::<ShellCommandOutputEvent>(event.payload.clone())
                {
                    if let Some(view) = self.command_view.as_mut()
                        && view.id == output.id
                        && output.size > view.cursor
                    {
                        view.wanted = true;
                    }
                    self.shell_commands.set_output_size(&output.id, output.size);
                }
                true
            }
            _ => false,
        }
    }

    /// The foreground command of a row worth the move hint: `{key} sends to
    /// background` once it has run a few seconds in the chat's own turn. A
    /// person's `!` command and a child agent's never move.
    pub(crate) fn move_hint(&self, row: &CommandRow, call_id: &str) -> Option<String> {
        if row.background
            || row.state != ShellCommandState::Running
            || self.is_child_view()
            || call_id.starts_with("shell-")
            || row.thread != self.conversation_id()
            || row
                .started_at
                .is_none_or(|started| Utc::now() - started < MOVE_HINT_AFTER)
        {
            return None;
        }
        let key = self
            .keybindings
            .get(BACKGROUND_ACTION)
            .map(String::as_str)
            .filter(|key| !key.is_empty() && *key != "none")?;
        Some(
            self.language
                .format("{key} sends to background", &[("key", key)]),
        )
    }

    /// `/background [id]` and its key: send the conversation's running
    /// foreground subagents and commands to the background, or the one
    /// named. The draft stays as it is.
    pub(crate) async fn send_to_background(
        &mut self,
        arguments: &str,
        client: &ApiClient,
    ) -> Result<()> {
        let chat = self.conversation_id();
        let commands = self.background_commands_supported();
        let named = arguments.trim();
        // Subagents are promoted by the chat that waits for them: the one on
        // screen (a child's own subagent, from its view), then the
        // conversation's top chat. Commands move for the whole conversation.
        let parents: Vec<ThreadId> = std::iter::once(self.thread.id)
            .chain((chat != self.thread.id).then_some(chat))
            .collect();
        let (subagents, moved) = if named.is_empty() {
            let mut subagents = 0;
            for parent in &parents {
                subagents += client.promote_background_tasks(*parent, None).await?.len();
            }
            let moved = if commands {
                match client.send_commands_to_background(chat, None).await {
                    Ok(moved) => moved.len(),
                    // The chat runs as many commands there as it may: the
                    // server says so in its own words.
                    Err(error)
                        if crate::api::refusal_status(&error)
                            == Some(reqwest::StatusCode::CONFLICT) =>
                    {
                        self.notice = error.to_string();
                        return Ok(());
                    }
                    Err(error) => return Err(error),
                }
            } else {
                0
            };
            (subagents, moved)
        } else {
            let child = named.parse::<uuid::Uuid>().ok();
            let mut subagents = 0;
            if let Some(child) = child {
                for parent in &parents {
                    subagents += client
                        .promote_background_tasks(*parent, Some(ThreadId(child)))
                        .await?
                        .len();
                    if subagents > 0 {
                        break;
                    }
                }
            }
            if subagents > 0 {
                (subagents, 0)
            } else if commands {
                match self.move_command(named, client).await? {
                    Some(moved) => (0, moved),
                    None => return Ok(()),
                }
            } else if child.is_none() {
                self.refuse_command("Usage: /background [running-child-thread-id]");
                return Ok(());
            } else {
                (0, 0)
            }
        };
        self.notice = match (subagents, moved) {
            (0, 0) if commands => self
                .language
                .text("Nothing is running in the foreground")
                .to_owned(),
            (0, 0) => "No matching foreground subagent is running".into(),
            (1, 0) => "Subagent moved to background · completion will return here".into(),
            (count, 0) => {
                format!("{count} subagents moved to background · completions will return here")
            }
            (0, 1) => self
                .language
                .text("Command moved to background · its result will return here")
                .to_owned(),
            (subagents, moved) => self.language.format(
                "{count} moved to background · results will return here",
                &[("count", &(subagents + moved).to_string())],
            ),
        };
        Ok(())
    }

    /// Send one command to the background: how many moved, or `None` once
    /// the notice says why it cannot.
    async fn move_command(&mut self, id: &str, client: &ApiClient) -> Result<Option<usize>> {
        let chat = self.conversation_id();
        match client.send_commands_to_background(chat, Some(id)).await {
            Ok(moved) => {
                for info in moved.iter().cloned() {
                    self.shell_commands.update(info, chat);
                }
                Ok(Some(moved.len()))
            }
            // Ended, already there, a subagent's, or the chat's cap: the
            // server's words say which.
            Err(error)
                if crate::api::refusal_status(&error) == Some(reqwest::StatusCode::CONFLICT) =>
            {
                let said = error.to_string();
                self.notice = if said.trim().is_empty() {
                    self.language
                        .text("This command can't move to the background")
                        .to_owned()
                } else {
                    said
                };
                Ok(None)
            }
            Err(error) if gone(&error) => Ok(Some(0)),
            Err(error) => Err(error),
        }
    }

    /// `/tasks` rows for the conversation's commands: those in the
    /// background, and the ones running in the foreground now.
    pub(crate) fn command_task_entries(&self) -> Vec<OptionEntry> {
        let language = self.language;
        self.shell_commands
            .of(self.conversation_id())
            .into_iter()
            .filter(|info| info.background || info.state.is_live())
            .map(|info| {
                let row = CommandRow::of_info(info);
                let state = if info.background {
                    row.status(language, false)
                } else {
                    row.elapsed().map_or_else(
                        || language.text("running").to_owned(),
                        |elapsed| format!("{} · {elapsed}", language.text("running")),
                    )
                };
                OptionEntry {
                    label: command_line(&info.command),
                    description: format!("{state} · {}", language.text("enter opens its output")),
                    value: format!("{TASKS_COMMAND}{}", info.id),
                }
            })
            .collect()
    }

    /// Open the full-screen view of a command's output and start reading it.
    pub(crate) fn open_command_output(&mut self, id: &str, client: Option<&ApiClient>) {
        let (thread, command) = self.shell_commands.get(id).map_or_else(
            || self.recorded_command(id),
            |info| (info.thread_id, info.command.clone()),
        );
        let mut view = CommandView::new(id.to_owned(), thread, command);
        if let Some(client) = client {
            view.read(client);
        }
        self.command_view = Some(view);
        self.open_overlay(Overlay::CommandOutput);
    }

    /// The chat and command line of a command known only from the
    /// transcript: the `bash` call, or `bash` step of a batch, whose answer
    /// or checkpoint names it. The line is empty when none does.
    pub(crate) fn recorded_command(&self, id: &str) -> (ThreadId, String) {
        // The call (and turn) that started it, and the item that says so.
        let started = self.items.iter().find_map(|item| match &item.data {
            ItemData::CommandState {
                command_id,
                call_id,
                ..
            } if command_id == id => Some((item, call_id.clone())),
            ItemData::ToolResult {
                call_id, metadata, ..
            } => {
                if named_command(metadata).is_some_and(|named| named.id == id) {
                    return Some((item, call_id.clone()));
                }
                // A batch's answer names each step's command in its entry.
                let step = metadata
                    .get("results")
                    .and_then(serde_json::Value::as_array)?
                    .iter()
                    .position(|entry| {
                        entry
                            .get("metadata")
                            .and_then(named_command)
                            .is_some_and(|named| named.id == id)
                    })?;
                Some((item, format!("{call_id}:{}", step + 1)))
            }
            _ => None,
        });
        let Some((item, call_id)) = started else {
            return (self.thread.id, String::new());
        };
        // `<batch call>:<step>` is a step of that batch call.
        let (call, step) = match call_id.rsplit_once(':') {
            Some((batch, step)) if step.parse::<usize>().is_ok() => {
                (batch.to_owned(), step.parse::<usize>().ok())
            }
            _ => (call_id, None),
        };
        let command = self
            .items
            .iter()
            .find_map(|candidate| match &candidate.data {
                ItemData::ToolCall {
                    call_id, arguments, ..
                } if *call_id == call && candidate.turn_id == item.turn_id => {
                    let arguments = match step {
                        Some(step) => arguments
                            .get("tool_calls")?
                            .get(step.checked_sub(1)?)?
                            .get("parameters")?,
                        None => arguments,
                    };
                    arguments
                        .get("command")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                }
                _ => None,
            })
            .unwrap_or_default();
        (item.thread_id, command)
    }

    /// What the output view shows when the server no longer has the
    /// command: the end its last checkpoint kept, else its call's answer.
    fn kept_output(&self, id: &str) -> String {
        if let Some(tail) = durable_states(&self.items).tail(id) {
            return tail.to_owned();
        }
        self.items
            .iter()
            .find_map(|item| match &item.data {
                ItemData::ToolResult { output, .. }
                    if result_command(Some(item)).is_some_and(|named| named.id == id) =>
                {
                    Some(output.clone())
                }
                _ => None,
            })
            .unwrap_or_default()
    }

    /// Take a finished read of the open output view and start the next one
    /// when there is more, or it is time to look again.
    pub(crate) fn poll_command_output(&mut self, client: &ApiClient) {
        let Some(mut view) = self.command_view.take() else {
            return;
        };
        if !matches!(self.overlay, Overlay::CommandOutput) {
            view.cancel();
            return;
        }
        if let Some(result) = view.take_read() {
            match result {
                Ok(page) => view.apply(page),
                Err(ReadError::Gone) => {
                    self.shell_commands.forget(&view.id);
                    if view.loaded {
                        // What was read stays; only no more comes.
                        view.settled = true;
                        view.state = view.state.filter(|(state, _)| !state.is_live());
                    } else {
                        let kept = self.kept_output(&view.id);
                        view.keep_only(&kept);
                    }
                }
                Err(_) => {}
            }
        }
        // Until every byte is in, an ended command's last output may still
        // be on its way: it is read again too.
        if view.pending.is_none()
            && !view.settled
            && (view.wanted || view.read_at.is_none_or(|at| at.elapsed() >= REREAD))
        {
            view.read(client);
        }
        self.command_view = Some(view);
    }

    /// The output view's keys: Esc or q goes back, PgUp/PgDn, ↑/↓, g and G
    /// scroll (End follows the end again), `s` stops the command and the
    /// background key sends a foreground one there.
    pub(crate) async fn command_output_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        client: &ApiClient,
    ) -> Result<()> {
        use crossterm::event::{KeyCode, KeyModifiers};
        if crate::keybindings::resolve(&self.keybindings, key) == Some(BACKGROUND_ACTION) {
            let movable = self.command_view.as_ref().and_then(|view| {
                self.shell_commands
                    .get(&view.id)
                    .filter(|info| !info.background && info.state.is_live())
                    .map(|info| info.id.clone())
            });
            if let Some(id) = movable
                && self.move_command(&id, client).await? == Some(1)
            {
                self.language
                    .text("Command moved to background · its result will return here")
                    .clone_into(&mut self.notice);
            }
            return Ok(());
        }
        let Some(view) = self.command_view.as_mut() else {
            self.close_overlay();
            return Ok(());
        };
        let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
        match key.code {
            KeyCode::Esc => self.close_overlay(),
            KeyCode::Char('q') if plain => self.close_overlay(),
            KeyCode::PageUp => view.scroll(-view.page.get().max(1).cast_signed()),
            KeyCode::PageDown => view.scroll(view.page.get().max(1).cast_signed()),
            KeyCode::Up | KeyCode::Char('k') if plain => view.scroll(-1),
            KeyCode::Down | KeyCode::Char('j') if plain => view.scroll(1),
            KeyCode::Home | KeyCode::Char('g') if plain => view.top = Some(0),
            KeyCode::End | KeyCode::Char('G') if plain => view.top = None,
            KeyCode::Char('s') if plain => {
                let id = view.id.clone();
                let thread = view.thread;
                let live = self
                    .shell_commands
                    .get(&id)
                    .is_some_and(|info| info.state == ShellCommandState::Running);
                if live {
                    match client.stop_shell_command(thread, &id).await {
                        Ok(info) => {
                            let conversation = self.conversation_id();
                            self.shell_commands.update(info, conversation);
                            if let Some(view) = self.command_view.as_mut() {
                                view.wanted = true;
                            }
                        }
                        // The server no longer has it: nothing runs to stop.
                        Err(error) if gone(&error) => {
                            self.shell_commands.forget(&id);
                            if let Some(view) = self.command_view.as_mut() {
                                view.settled = true;
                                view.state = None;
                            }
                        }
                        // A relay or a Cloud workspace that sleeps answers
                        // for a while with something else: the view stays.
                        Err(error) => return Err(error),
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Mouse wheel over the output view.
    pub(crate) fn wheel_command_output(&mut self, up: bool) {
        if let Some(view) = self.command_view.as_mut() {
            view.scroll(if up { -3 } else { 3 });
        }
    }
}

/// What a `bash_output` or `bash_stop` row names: the command it reads or
/// stops, by its line rather than its id, and the line a wait looks for.
pub(crate) fn command_tool_target(
    app: &App,
    name: &str,
    arguments: &serde_json::Value,
) -> Option<String> {
    if !matches!(name, "bash_output" | "bash_stop") {
        return None;
    }
    // By its line, never its id: an id a person cannot read leaves the row
    // to name what else the call says.
    let id = arguments.get("id").and_then(serde_json::Value::as_str)?;
    let line = app.shell_commands.get(id).map_or_else(
        || command_line(&app.recorded_command(id).1),
        |info| command_line(&info.command),
    );
    if line.is_empty() {
        return None;
    }
    let pattern = arguments
        .get("pattern")
        .and_then(serde_json::Value::as_str)
        .filter(|pattern| !pattern.is_empty() && name == "bash_output");
    Some(match pattern {
        Some(pattern) => format!("{line} · {}", crate::escapes::spelled_out(pattern)),
        None => line,
    })
}

/// The first line of a command, as a row or a list names it.
pub(crate) fn command_line(command: &str) -> String {
    let first = command.lines().next().unwrap_or_default().trim();
    if command.trim().lines().nth(1).is_some() {
        format!("{first} …")
    } else {
        first.to_owned()
    }
}

/// Only these answers say the server does not have a command: anything else
/// on a command's route (a relay's 409 or 400, a Cloud workspace waking up)
/// passes, and what is on screen stays.
fn gone(error: &anyhow::Error) -> bool {
    matches!(
        crate::api::refusal_status(error),
        Some(reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::GONE)
    )
}

/// Why a read of a command's output failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReadError {
    /// The server no longer has the command (or never had it).
    Gone,
    /// Anything else; the next read tries again.
    Failed,
}

type Read = tokio::sync::oneshot::Receiver<std::result::Result<ShellCommandOutput, ReadError>>;

/// Rows of the output at a width, for the revision of the output they were
/// cut from.
type ShownRows = Option<(u64, usize, Rc<[String]>)>;

/// The full-screen view of one command's output (`Overlay::CommandOutput`).
#[allow(clippy::struct_excessive_bools)] // independent facts about one read stream
pub(crate) struct CommandView {
    pub id: String,
    /// The chat whose routes reach the command.
    pub thread: ThreadId,
    /// Its command line, for the header when the server no longer has it.
    pub command: String,
    /// What it printed, as written (escapes left in), from where the view
    /// started reading.
    raw: String,
    /// Grows with every change to `raw`, which can keep its length while
    /// its text moves on (output past `OUTPUT_KEPT`).
    revision: u64,
    /// The next read starts from the end again: the output ran on further
    /// than the view keeps while it followed the end.
    skip_ahead: bool,
    /// The read in flight is such a read from the end.
    skipping: bool,
    /// Where the next read starts.
    pub cursor: u64,
    /// The command has ended and every byte it printed is in.
    pub settled: bool,
    /// What the output page last said about the command, for a command
    /// this client has no report of.
    pub state: Option<(ShellCommandState, Option<i32>)>,
    /// Output before what is shown is no longer kept.
    pub trimmed: bool,
    /// The server no longer has the command: only its kept end shows.
    pub only_kept: bool,
    /// A first read came back.
    pub loaded: bool,
    /// The first row shown, once scrolled; `None` follows the end.
    pub top: Option<usize>,
    /// Rows the output came to, and rows on screen, in the last frame.
    pub rows: Cell<usize>,
    pub page: Cell<usize>,
    /// Read again as soon as the read in flight is in.
    pub wanted: bool,
    /// When the view opened: its first read usually lands before anything
    /// needs saying.
    pub opened: Instant,
    read_at: Option<Instant>,
    pending: Option<(Read, tokio::task::JoinHandle<()>)>,
    /// Rows of the output at a width, kept until it changes.
    shown: RefCell<ShownRows>,
}

impl CommandView {
    pub(crate) fn new(id: String, thread: ThreadId, command: String) -> Self {
        Self {
            id,
            thread,
            command,
            raw: String::new(),
            revision: 0,
            skip_ahead: false,
            skipping: false,
            cursor: 0,
            settled: false,
            state: None,
            trimmed: false,
            only_kept: false,
            loaded: false,
            top: None,
            rows: Cell::new(0),
            page: Cell::new(0),
            wanted: true,
            opened: Instant::now(),
            read_at: None,
            pending: None,
            shown: RefCell::new(None),
        }
    }

    /// Start the next read: the end of the output first, then on from it.
    fn read(&mut self, client: &ApiClient) {
        if self.pending.is_some() {
            return;
        }
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let client = client.clone();
        let (thread, id) = (self.thread, self.id.clone());
        // The end first, then on from it; from the end again when following
        // fell further behind than the view keeps.
        let (cursor, tail) = if self.loaded && !self.skip_ahead {
            (Some(self.cursor), false)
        } else {
            (None, true)
        };
        self.skipping = self.loaded && tail;
        self.skip_ahead = false;
        let task = tokio::spawn(async move {
            let result = client
                .shell_command_output(thread, &id, cursor, tail, OUTPUT_PAGE)
                .await
                .map_err(|error| {
                    if gone(&error) {
                        ReadError::Gone
                    } else {
                        tracing::debug!(%error, "could not read a command's output");
                        ReadError::Failed
                    }
                });
            let _ = sender.send(result);
        });
        self.pending = Some((receiver, task));
        self.wanted = false;
        self.read_at = Some(Instant::now());
    }

    fn take_read(&mut self) -> Option<std::result::Result<ShellCommandOutput, ReadError>> {
        let (receiver, _) = self.pending.as_mut()?;
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return None,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => Err(ReadError::Failed),
        };
        self.pending = None;
        Some(result)
    }

    fn cancel(&mut self) {
        if let Some((_, task)) = self.pending.take() {
            task.abort();
        }
    }

    /// Take a page of output.
    pub(crate) fn apply(&mut self, page: ShellCommandOutput) {
        if !self.loaded {
            self.trimmed = page.start > 0;
            self.loaded = true;
        } else if std::mem::take(&mut self.skipping) {
            // A read from the end: what came before it is no longer shown.
            if page.start > self.cursor {
                self.raw.clear();
                self.trimmed = true;
            } else {
                // Nothing was skipped after all: keep what follows the cursor.
                let fresh = usize::try_from(self.cursor.saturating_sub(page.start)).unwrap_or(0);
                let mut cut = fresh.min(page.text.len());
                while !page.text.is_char_boundary(cut) {
                    cut += 1;
                }
                let mut page = page;
                page.text.drain(..cut);
                return self.take_page(page);
            }
        } else if page.start > self.cursor {
            // Output between the last read and this one is no longer kept.
            self.trimmed = true;
            if !self.raw.is_empty() && !self.raw.ends_with('\n') {
                self.raw.push('\n');
            }
        }
        self.take_page(page);
    }

    fn take_page(&mut self, page: ShellCommandOutput) {
        self.revision += page.text.len() as u64 + 1;
        self.raw.push_str(&page.text);
        if self.raw.len() > OUTPUT_KEPT {
            let mut cut = self.raw.len() - OUTPUT_KEPT;
            while !self.raw.is_char_boundary(cut) {
                cut += 1;
            }
            let cut = self.raw[cut..]
                .find('\n')
                .map_or(cut, |line| cut + line + 1);
            self.raw.drain(..cut);
            self.trimmed = true;
        }
        self.cursor = page.cursor;
        self.settled = page.settled;
        self.state = Some((page.state, page.exit_code));
        self.wanted |= page.more;
        // Following the end, output further ahead than the view keeps is
        // read from the end rather than page by page: the screen keeps up.
        if self.top.is_none() && page.size.saturating_sub(page.cursor) > OUTPUT_KEPT as u64 {
            self.skip_ahead = true;
            self.wanted = true;
        }
    }

    /// The server no longer has the command: show the end that was kept.
    fn keep_only(&mut self, kept: &str) {
        kept.clone_into(&mut self.raw);
        self.revision += 1;
        self.loaded = true;
        self.settled = true;
        self.only_kept = true;
    }

    fn scroll(&mut self, rows: isize) {
        let page = self.page.get();
        let last = self.rows.get().saturating_sub(page);
        let start = self.top.unwrap_or(last).min(last);
        let next = start.saturating_add_signed(rows);
        self.top = (next < last).then_some(next);
    }

    /// The output as rows of at most `width` columns, escapes left out and
    /// a line rewritten with carriage returns as it last read.
    pub(crate) fn shown_rows(&self, width: usize) -> Rc<[String]> {
        let width = width.max(8);
        if let Some((revision, at, rows)) = self.shown.borrow().as_ref()
            && *revision == self.revision
            && *at == width
        {
            return rows.clone();
        }
        let clean = crate::escapes::without_escapes(&self.raw);
        let mut rows = Vec::new();
        let text = clean.strip_suffix('\n').unwrap_or(&clean);
        if !text.is_empty() {
            for line in text.split('\n') {
                let line = line.strip_suffix('\r').unwrap_or(line);
                let line = line.rsplit('\r').next().unwrap_or(line);
                wrap_into(&mut rows, line, width);
            }
        }
        let rows: Rc<[String]> = rows.into();
        *self.shown.borrow_mut() = Some((self.revision, width, rows.clone()));
        rows
    }
}

/// Cut a line of output into rows of `width` columns, tabs set every eight.
fn wrap_into(rows: &mut Vec<String>, line: &str, width: usize) {
    let mut row = String::new();
    let mut used = 0;
    for character in line.chars() {
        if character == '\t' {
            let stop = (used / 8 + 1) * 8;
            if stop > width {
                rows.push(std::mem::take(&mut row));
                used = 0;
                continue;
            }
            row.extend(std::iter::repeat_n(' ', stop - used));
            used = stop;
            continue;
        }
        let cells = character.width().unwrap_or(0);
        if used + cells > width {
            rows.push(std::mem::take(&mut row));
            used = 0;
        }
        row.push(character);
        used += cells;
    }
    rows.push(row);
}

/// What the output view's header says about the command's state, and the
/// task state its colour comes from.
pub(crate) fn view_status(app: &App, view: &CommandView) -> (String, &'static str) {
    let language = app.language;
    if let Some(info) = app.shell_commands.get(&view.id) {
        let row = CommandRow::of_info(info);
        if info.background {
            return (row.status(language, false), row.task_state());
        }
        return match row.ending(language) {
            Some(ending) => (ending, row.task_state()),
            None => (
                row.elapsed().map_or_else(
                    || language.text("running").to_owned(),
                    |elapsed| format!("{} · {elapsed}", language.text("running")),
                ),
                "running",
            ),
        };
    }
    match view.state {
        Some((state, exit_code)) => (
            ending(language, state, exit_code, None)
                .unwrap_or_else(|| language.text("running").to_owned()),
            task_state(state, exit_code, None),
        ),
        None => (String::new(), "unknown"),
    }
}

/// Whether the command the view shows can still be stopped.
pub(crate) fn view_can_stop(app: &App, view: &CommandView) -> bool {
    app.shell_commands
        .get(&view.id)
        .is_some_and(|info| info.state == ShellCommandState::Running)
}

#[cfg(test)]
pub(crate) fn fixture_info(
    id: &str,
    thread: ThreadId,
    call_id: &str,
    command: &str,
    background: bool,
    state: ShellCommandState,
) -> ShellCommandInfo {
    ShellCommandInfo {
        id: id.into(),
        thread_id: thread,
        root_thread_id: thread,
        turn_id: haarness_protocol::TurnId::new(),
        call_id: call_id.into(),
        command: command.into(),
        workdir: None,
        background,
        moved_by: background.then_some(haarness_protocol::ShellCommandMovedBy::Agent),
        state,
        exit_code: None,
        signal: None,
        ended_by: None,
        started_at: Utc::now(),
        background_at: background.then(Utc::now),
        ended_at: (!state.is_live()).then(Utc::now),
        stops_at: None,
        moves_at: None,
        output_bytes: 0,
        delivers_notice: background,
    }
}
