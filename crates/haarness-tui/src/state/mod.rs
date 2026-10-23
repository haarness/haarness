//! Client-side application state. Everything here mirrors server state or
//! holds pure UI concerns; nothing talks to the runtime directly.

mod completion;
mod reduce;

#[cfg(test)]
pub use completion::COMPLETION_GRACE;
pub use completion::TurnEnd;

use haarness_protocol::{
    AgentProfile, Item, ItemData, ItemId, PendingPermissionInfo, PendingQuestionInfo, ProviderInfo,
    Thread, ThreadId, Turn, TurnId, WorkflowDefinition, WorkflowInvalidDefinition, WorkflowRun,
    WorkflowRunStatus, WorkspaceListResponse, WorkspaceRefsResponse,
};
use unicode_width::UnicodeWidthChar as _;

use crate::commands::COMMANDS;
use crate::input::{WorkspaceMutation, WorkspaceRefOperation, composer::Composer};
use crate::theme::Theme;

/// How long a status-line notice stays once it has appeared.
pub(crate) const NOTICE_TTL: std::time::Duration = std::time::Duration::from_secs(8);
/// A notice younger than this is still the answer to the user's own action.
const FRESH_NOTICE: std::time::Duration = std::time::Duration::from_secs(4);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InterruptTarget {
    Turn(TurnId),
    Session(ThreadId),
    Quit,
}

/// One chip on the docked approval panel, in display order. The key is the
/// hotkey that has always answered prompts; chips and hotkeys resolve to the
/// same reply path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionChoice {
    Once,
    Auto,
    Always,
    Workspace,
    Repository,
    Reject,
}

impl PermissionChoice {
    /// The chips the approval panel offers, in display order. While an
    /// "always" or Once + Auto answer awaits its confirming press the panel
    /// shows confirm/cancel instead, so callers must check `confirm_always`
    /// first.
    pub const ALL: [Self; 5] = [
        Self::Once,
        Self::Always,
        Self::Workspace,
        Self::Reject,
        Self::Auto,
    ];

    pub fn available(app: &App) -> Vec<Self> {
        if app
            .first_pending_permission()
            .is_some_and(|pending| pending.allow_always == Some(false))
        {
            return vec![Self::Once, Self::Reject];
        }
        // Chrome site access is granted to one chat, never to a folder or a
        // repository, and never by switching the chat to Auto. Allowing it
        // in this chat is the answer people mean, so it comes first and is
        // what a bare Enter picks.
        if app
            .first_pending_permission()
            .is_some_and(|pending| pending.explicit)
        {
            return vec![Self::Always, Self::Once, Self::Reject];
        }
        let mut choices = Self::ALL.to_vec();
        if app
            .first_pending_permission()
            .is_some_and(|pending| pending.repository_scope.is_some())
        {
            choices.insert(3, Self::Repository);
        }
        // A chat already in Auto (its reviewer left this one to a person)
        // has nothing to switch to.
        if app.auto_permissions {
            choices.retain(|choice| *choice != Self::Auto);
        }
        choices
    }

    #[must_use]
    pub const fn key(self) -> char {
        match self {
            Self::Once => 'y',
            Self::Auto => 'u',
            Self::Always => 'a',
            Self::Workspace => 'w',
            Self::Repository => 'r',
            Self::Reject => 'n',
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Once => "Allow once",
            Self::Auto => "Once + Auto",
            Self::Always => "Always",
            Self::Workspace => "This folder",
            Self::Repository => "Repository",
            Self::Reject => "Reject",
        }
    }

    /// The chip's words for an `explicit` (Chrome site access) request, where
    /// Always means "in this chat" and a no is a denial of that site.
    #[must_use]
    pub const fn label_for(self, explicit: bool) -> &'static str {
        match self {
            Self::Always if explicit => "Allow in this chat",
            Self::Reject if explicit => "Deny",
            other => other.label(),
        }
    }

    /// Answers that reach past this one request (a standing rule, or Auto for
    /// the whole chat) wait for a second, confirming press.
    #[must_use]
    pub const fn staged(self) -> bool {
        matches!(
            self,
            Self::Always | Self::Workspace | Self::Repository | Self::Auto
        )
    }
}

/// Where the approval card's subject (a long command, say) is scrolled to,
/// and whether it is expanded, for the request it was moved on: any other
/// request starts at its first row, collapsed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionScroll {
    pub permission_id: String,
    /// First subject row shown; the card clamps it to what it can show.
    pub offset: usize,
    /// The subject takes every row the card can spare, not just its first
    /// few.
    pub expanded: bool,
}

/// How the approval card's subject fitted the last frame, so the keys that
/// move it act on what the person saw: nothing to scroll means arrows do
/// nothing, and expanding is offered only where it shows more.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SubjectFit {
    /// Largest useful offset.
    pub max_offset: usize,
    /// Subject rows on screen; page keys move by this much.
    pub shown: usize,
    /// Expanding would show more rows than the collapsed card does.
    pub expandable: bool,
}

/// Live progress of one child-agent task, keyed by its `task` call id and fed
/// by `task.updated` events from the server.
#[derive(Clone)]
pub struct ChildTask {
    pub child_thread_id: ThreadId,
    pub child_turn_id: Option<TurnId>,
    /// Most recent root turn that invoked or continued this child. The
    /// sidebar is a turn-local activity surface, while the child thread
    /// itself remains durable and navigable after this row is hidden.
    pub invoked_turn_id: Option<TurnId>,
    /// running | background | waiting_permission | done | failed | cancelled |
    /// interrupted
    pub state: String,
    pub tool_calls: u64,
    pub last_tool: Option<String>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl ChildTask {
    pub fn is_active(&self) -> bool {
        !matches!(
            self.state.as_str(),
            "done" | "failed" | "cancelled" | "interrupted"
        )
    }

    pub fn elapsed_seconds(&self) -> Option<i64> {
        if !matches!(
            self.state.as_str(),
            "running" | "background" | "waiting_permission"
        ) {
            return None;
        }
        self.started_at
            .map(|started| (chrono::Utc::now() - started).num_seconds().max(0))
    }
}

/// Whether a child belongs to the background execution lane. Live
/// `task.updated` progress deliberately reports the child's instantaneous
/// lifecycle (`running`, `waiting_permission`, ...), not how the parent
/// launched it. Recover that durable mode from the parent transcript so a
/// later progress event cannot make a background task look foreground.
pub(super) fn task_runs_in_background(
    items: &[Item],
    call_id: &str,
    child_thread_id: ThreadId,
) -> bool {
    items.iter().any(|item| match &item.data {
        ItemData::ToolCall {
            call_id: candidate,
            name,
            arguments,
            ..
        } => {
            candidate == call_id
                && name == "task"
                && arguments
                    .get("background")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
        }
        ItemData::ToolResult {
            call_id: candidate,
            name,
            output,
            is_error,
            ..
        } => {
            candidate == call_id
                && name == "task"
                && !is_error
                && output.contains("state=\"running\"")
        }
        ItemData::TaskState {
            call_id: candidate,
            child_thread_id: candidate_child,
            state,
            ..
        } => state == "background" && candidate == call_id && *candidate_child == child_thread_id,
        _ => false,
    })
}

fn durable_children(items: &[Item]) -> std::collections::HashMap<String, ChildTask> {
    let mut children = std::collections::HashMap::<String, ChildTask>::new();
    let mut child_rows = std::collections::HashMap::<ThreadId, String>::new();
    let mut call_rows = std::collections::HashMap::<String, String>::new();

    for item in items {
        match &item.data {
            ItemData::TaskLink {
                call_id,
                child_thread_id,
            } => {
                let call = items.iter().find(|candidate| {
                    matches!(
                        &candidate.data,
                        ItemData::ToolCall {
                            call_id: candidate_call_id,
                            name,
                            ..
                        } if candidate_call_id == call_id && name == "task"
                    )
                });
                let invoked_turn_id = call.and_then(|call| call.turn_id).or(item.turn_id);
                let row = child_rows
                    .entry(*child_thread_id)
                    .or_insert_with(|| call_id.clone())
                    .clone();
                call_rows.insert(call_id.clone(), row.clone());
                if let Some(task) = children.get_mut(&row) {
                    // A continuation is activity by the same durable child,
                    // not a second agent. Keep its original identity row but
                    // move that row into the turn that invoked it again.
                    task.invoked_turn_id = invoked_turn_id.or(task.invoked_turn_id);
                } else {
                    let background = task_runs_in_background(items, call_id, *child_thread_id);
                    children.insert(
                        row,
                        ChildTask {
                            child_thread_id: *child_thread_id,
                            child_turn_id: None,
                            invoked_turn_id,
                            state: if background { "background" } else { "running" }.into(),
                            tool_calls: 0,
                            last_tool: None,
                            started_at: Some(call.map_or(item.created_at, |call| call.created_at)),
                        },
                    );
                }
            }
            ItemData::ToolResult {
                call_id,
                name,
                output,
                is_error,
                ..
            } if name == "task" => {
                let Some(row) = call_rows.get(call_id) else {
                    continue;
                };
                let Some(task) = children.get_mut(row) else {
                    continue;
                };
                task.state = if !is_error && output.contains("state=\"running\"") {
                    "background"
                } else if !is_error {
                    "done"
                } else if output.trim() == "operation cancelled" {
                    "cancelled"
                } else {
                    "failed"
                }
                .into();
            }
            ItemData::TaskState {
                child_thread_id,
                child_turn_id,
                state,
                ..
            } => {
                if let Some(row) = child_rows.get(child_thread_id)
                    && let Some(task) = children.get_mut(row)
                {
                    task.child_turn_id = Some(*child_turn_id);
                    task.state.clone_from(state);
                }
            }
            _ => {}
        }
    }
    children
}

fn durable_todos(items: &[Item], through: Option<u64>) -> Vec<haarness_protocol::TodoEntry> {
    items
        .iter()
        .rev()
        .find_map(|item| {
            (through.is_none_or(|sequence| item.sequence <= sequence))
                .then_some(&item.data)
                .and_then(|data| match data {
                    ItemData::Todo { items } => Some(items.clone()),
                    _ => None,
                })
        })
        .unwrap_or_default()
}

/// A batch result's entry: its tool, whether it failed, and, when Auto's
/// reviewer refused it, the reason it gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchOutcome {
    pub tool: String,
    pub failed: bool,
    /// Auto's reviewer refused the step, with its note.
    pub blocked: Option<haarness_protocol::PermissionReview>,
}

/// A batch result's entries, in step order.
pub type BatchOutcomes = std::rc::Rc<[BatchOutcome]>;

/// What the running turn is doing right now, mirrored from `turn.status`
/// events — the status line renders this instead of a stale free-text notice.
#[derive(Clone, Default)]
pub struct PhaseInfo {
    /// thinking | responding | tool | reviewing (Auto's reviewer) |
    /// waiting_permission | waiting_question | child_tasks | compacting | done
    pub state: String,
    pub tool: Option<String>,
    pub call_id: Option<String>,
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    /// When this client heard of the phase: a delay before saying what it
    /// is runs on this clock, never the server's.
    pub received: Option<std::time::Instant>,
}

/// What a chat waits on from a person. An approval outranks a question.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Waiting {
    Answer,
    Approval,
}

/// A permission request or question that waits for a person, in any chat on
/// the server: the thread that asked and its conversation's root when the
/// server named one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitingRequest {
    pub thread_id: ThreadId,
    pub root_thread_id: Option<ThreadId>,
    pub kind: Waiting,
}

/// In-flight rendering state belongs to its thread. Keeping it keyed avoids
/// dropping a streaming prefix when the user opens a child agent and returns
/// before the durable assistant item is written.
#[derive(Clone, Default)]
pub struct LiveThreadState {
    pub preview: String,
    pub reasoning_preview: String,
    pub phase: Option<PhaseInfo>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ServerRestart {
    Starting,
    Failed,
}

/// An undone prompt set aside by a slash command typed over it. It returns
/// to the composer once the command is done or erased, except after `/redo`,
/// which brings the turn itself back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UndoDraft {
    pub text: String,
    pub attachments: Vec<String>,
}

impl PhaseInfo {
    pub fn elapsed_seconds(&self) -> i64 {
        self.since
            .map_or(0, |since| (chrono::Utc::now() - since).num_seconds().max(0))
    }

    /// How long ago this client heard of the phase.
    pub fn heard_millis(&self) -> u128 {
        self.received
            .map_or(0, |received| received.elapsed().as_millis())
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum Overlay {
    None,
    Commands,
    Threads,
    Models,
    Agents,
    Themes,
    /// Generic option picker backing argument-style commands.
    Options {
        title: String,
        kind: OptionsKind,
        entries: Vec<OptionEntry>,
    },
    Panel {
        title: String,
        lines: Vec<String>,
    },
    /// Full-screen review of Git and completed-turn changes: file tree,
    /// stacked or side-by-side patches, line cursor, and queued comments.
    Diff(Box<crate::diffview::DiffViewer>),
    /// One workflow, re-read from the cached catalog on every frame so a
    /// run's phases, agents, and logs stay live while it is open.
    Workflow {
        target: WorkflowTarget,
    },
    /// A provider sign-in in progress; the state lives in `App::connect`.
    ProviderLogin,
    /// `/remote-control`; the state lives in `App::remote`.
    RemoteControl,
    /// `/chrome`; the state lives in `App::chrome`.
    Chrome,
    /// `/btw`: the open chat's side chat; the state lives in `App::side`.
    SideChat,
}

/// What the workflow inspector is showing: a durable run, or a definition you
/// have not started yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowTarget {
    Run(haarness_protocol::WorkflowId),
    /// Keyed by invocation, the catalog's stable identifier.
    Definition(String),
    /// A file that failed to parse, keyed by its index in the catalog.
    Invalid(usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OptionsKind {
    CloudRouter,
    /// `/billing`: the Cloud plan and balances, with what can be done next.
    Billing,
    InputQueue,
    InputQueueActions,
    Management,
    ManagementField,
    ManagementValue,
    ManagementConfirm,
    /// `/connect`: which provider to connect.
    ConnectProvider,
    /// `/connect`: sign in, or paste a key, for the chosen provider.
    ConnectMethod,
    /// `/connect`: the masked API-key prompt.
    ConnectApiKey,
    ReasoningEffort,
    Language,
    /// Searchable project/workspace dashboard opened by bare `/workspace`.
    Workspaces,
    /// Context-sensitive controls for one selected workspace.
    WorkspaceActions,
    /// First step of workspace creation: the required display name.
    WorkspaceCreateName,
    /// Second step of workspace creation: an optional Git base ref.
    WorkspaceCreateBase,
    /// Arbitrary branch/tag/commit prompt reached from the base-ref picker.
    WorkspaceCreateCustomBase,
    /// Cancel-first confirmation for resetting a managed worktree.
    WorkspaceResetConfirmation,
    /// Cancel-first confirmation for removing a managed worktree.
    WorkspaceRemoveConfirmation,
    /// Workspace tool catalog; choosing a row toggles durable availability.
    Tools,
    /// Child-agent tasks; choosing one opens its live thread view.
    Tasks,
    /// Actions for the session selected in the session switcher.
    SessionActions,
    /// Combined recent-run and discovered-definition workflow dashboard.
    WorkflowDashboard,
    /// Context-sensitive actions for one durable workflow run.
    WorkflowRunActions,
    /// Standing-goal status and lifecycle controls.
    GoalActions,
    /// Destructive goal clearing uses a separate, cancel-first confirmation.
    GoalClearConfirmation,
    /// One-shot choices surfaced after the model completes a goal.
    GoalCompletion,
    /// Undoable folder checkpoints, newest first.
    Timeline,
    /// Assistant replies which form inclusive message fork boundaries.
    ForkMessages,
    Messages,
    MessageActions,
}

#[derive(Clone, PartialEq, Eq)]
pub struct OptionEntry {
    pub label: String,
    pub description: String,
    pub value: String,
}

impl Overlay {
    pub const fn is_selector(&self) -> bool {
        matches!(
            self,
            Self::Commands
                | Self::Threads
                | Self::Models
                | Self::Agents
                | Self::Themes
                | Self::Options { .. }
        )
    }

    pub const fn is_filterable(&self) -> bool {
        match self {
            Self::Commands | Self::Threads | Self::Models | Self::Agents | Self::Themes => true,
            Self::Options { kind, .. } => !matches!(
                kind,
                OptionsKind::ManagementConfirm
                    | OptionsKind::Billing
                    | OptionsKind::ConnectMethod
                    | OptionsKind::WorkspaceActions
                    | OptionsKind::WorkspaceResetConfirmation
                    | OptionsKind::WorkspaceRemoveConfirmation
                    | OptionsKind::GoalActions
                    | OptionsKind::GoalClearConfirmation
                    | OptionsKind::GoalCompletion
            ),
            Self::None
            | Self::Panel { .. }
            | Self::Diff(_)
            | Self::Workflow { .. }
            | Self::ProviderLogin
            | Self::RemoteControl
            | Self::Chrome
            | Self::SideChat => false,
        }
    }
}

/// One row in the slash-command suggestion list (built-in or server-defined).
#[derive(Clone, PartialEq, Eq)]
pub struct SlashEntry {
    pub name: String,
    pub description: String,
}

pub enum OverlayChoice {
    Command(String),
    Thread(Box<Thread>),
    Model(String),
    Agent(String),
    Theme(String, String),
    Option(OptionsKind, String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastVariant {
    Success,
    Warning,
    Error,
}

/// A queued follow-up loaded into the prompt for editing. Whatever the prompt
/// held before is set aside here, whole, and comes back on save or cancel.
pub struct QueueEdit {
    pub id: haarness_protocol::RequestId,
    /// The message as it was when the edit began. A save never overwrites a
    /// version another client wrote meanwhile.
    pub original: (String, Vec<String>),
    pub draft: crate::input::composer::SetAside,
    pub attachments: Vec<String>,
    pub shell: bool,
    /// Why the last save was refused. The editing line shows it until the
    /// next attempt, because a running turn's band has no room for notices.
    pub conflict: Option<String>,
}

#[derive(Clone)]
pub struct Toast {
    pub title: Option<String>,
    pub message: String,
    pub variant: ToastVariant,
    pub expires_at: std::time::Instant,
    /// Repeats the approval card. It stays hidden while the card itself is on
    /// screen, and only shows where a dialog or a small terminal hides it.
    pub approval: bool,
}

/// A context estimate keyed by the shown thread's item count, last sequence
/// and undo boundary, with the tokens it came to.
pub type ContextEstimate = ((usize, u64, Option<u64>), u64);

/// How the model's reasoning shows in the transcript; Ctrl+T cycles them in
/// this order, and the choice is kept for the next launch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThinkingMode {
    /// One row while the model thinks, with its latest line, that becomes
    /// `Thought for 12s` and opens on a click: the transcript never jumps.
    #[default]
    Compact,
    /// The reasoning in full.
    Show,
    /// None of it.
    Hide,
}

impl ThinkingMode {
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Compact => Self::Show,
            Self::Show => Self::Hide,
            Self::Hide => Self::Compact,
        }
    }
    /// The word saved in `tui-state.toml`.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Show => "show",
            Self::Hide => "hide",
        }
    }
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        [Self::Compact, Self::Show, Self::Hide]
            .into_iter()
            .find(|mode| mode.key() == value)
    }
    /// Its name as the interface shows it.
    #[must_use]
    pub fn label(self, language: crate::locale::Language) -> &'static str {
        match self {
            Self::Compact => language.text("Compact"),
            Self::Show => language.text("Show"),
            Self::Hide => language.text("Hide"),
        }
    }
}

#[allow(clippy::struct_excessive_bools)] // independent UI toggles, not a state machine
pub struct App {
    pub input_queue: haarness_protocol::InputQueue,
    /// The queued message whose action menu is open, with the queue revision
    /// the menu was built from.
    pub input_queue_edit: Option<(haarness_protocol::RequestId, u64)>,
    /// The action menu was reached through the `/queue` list, so finishing an
    /// action returns there instead of closing.
    pub input_queue_listed: bool,
    /// A queued message open for editing in the prompt.
    pub queue_edit: Option<QueueEdit>,
    pub(crate) pending_server: Option<Box<crate::servers::Connection>>,
    pub(crate) original_client: Option<crate::api::ApiClient>,
    pub(crate) keybindings: std::collections::BTreeMap<String, String>,
    /// Shortcut settings left out at launch (unknown, unbindable, or a key
    /// taken by another shortcut), said once on the status line.
    pub(crate) keybindings_skipped: Vec<String>,
    pub(crate) management: crate::management::Management,
    pub(crate) connect: crate::connect::ConnectFlow,
    /// `/remote-control`: whether this chat is shared through Haarness Cloud.
    pub(crate) remote: crate::remote_control::RemoteControl,
    /// `/chrome`: whether agents can use the person's own Chrome.
    pub(crate) chrome: crate::chrome::Chrome,
    /// `/logs`: an export or a change of detailed logging in flight.
    pub(crate) logs: crate::logs::Logs,
    /// `/btw`: each chat's side chat.
    pub(crate) side: crate::side_chat::SideChats,
    pub thread: Thread,
    pub threads: Vec<Thread>,
    /// Whether the current session catalog includes archived conversations.
    /// Normal refreshes reset this so archived sessions do not leak back into
    /// the everyday switcher after `/sessions --all`.
    pub session_include_archived: bool,
    pub items: Vec<Item>,
    /// Canonical current todo state, hydrated independently from transcript
    /// history so revert/unrevert and reconnects render the right snapshot.
    pub todos: Vec<haarness_protocol::TodoEntry>,
    pub model_favorites: Vec<String>,
    pub model_recents: Vec<String>,
    pub providers: Vec<ProviderInfo>,
    pub agents: Vec<AgentProfile>,
    pub agent_index: usize,
    pub composer: Composer,
    /// Unsent prompt/media state follows its thread instead of disappearing
    /// or being sent into whichever session the user opens next.
    pub(crate) drafts: std::collections::HashMap<ThreadId, crate::servers::Draft>,
    pub(crate) draft_baseline:
        std::cell::RefCell<std::collections::HashMap<ThreadId, crate::servers::Draft>>,
    /// Media waiting to be sent with the next user message.
    pub attachments: Vec<String>,
    pub preview: String,
    pub reasoning_preview: String,
    pub live_threads: std::collections::HashMap<ThreadId, LiveThreadState>,
    /// Every request waiting for a person on the server, by request id. The
    /// session list leads with the chats they wait in.
    pub waiting: std::collections::HashMap<String, WaitingRequest>,
    pub notice: String,
    /// The notice last seen by the render tick and when it appeared, so an
    /// answer to something the user did fades instead of describing the past.
    pub(crate) notice_shown: Option<(String, std::time::Instant)>,
    /// The prompt `/undo` put back in the composer, while a slash command
    /// typed over it holds its place.
    pub(crate) undo_draft: Option<UndoDraft>,
    /// First non-empty line produced by the configured status-line command.
    pub custom_status_line: Option<String>,
    pub billing: haarness_account::billing::State,
    pub language: crate::locale::Language,
    /// SSE health: while present, stale activity is explicitly labelled as
    /// reconnecting rather than looking like a hung model.
    pub connection_lost: Option<u32>,
    /// The provider catalog did not come in time for the first frame: it
    /// says "Loading models…", not that nothing is connected.
    pub(crate) models_loading: bool,
    /// Where a provider catalog read off the event loop goes (see
    /// `App::catalog_arrived`); without it, the read waits in place.
    pub(crate) catalog_tx:
        Option<tokio::sync::mpsc::Sender<Option<Vec<haarness_protocol::ProviderInfo>>>>,
    /// When the "Connection lost" toast was last shown.
    pub(crate) lost_toast_at: Option<std::time::Instant>,
    /// The task following the server's events kept ending by itself: it
    /// is tried again in a minute, and nothing retries meanwhile.
    pub(crate) following_stopped: bool,
    /// The chat this TUI opened on a model it chose while the catalog was
    /// loading: its id and that model, re-chosen once the catalog comes.
    pub(crate) auto_model: Option<(ThreadId, String, String)>,
    /// While disconnected: whether this client is bringing its local server
    /// back, or failed to.
    pub(crate) server_restart: Option<ServerRestart>,
    pub overlay: Overlay,
    pub thinking: ThinkingMode,
    pub active_turn: Option<TurnId>,
    pub refresh_needed: bool,
    /// Root of the conversation this view follows: the current thread for
    /// root threads, the top of the parent chain when drilled into a child.
    pub root_thread_id: ThreadId,
    /// The root thread's running turn, tracked even while viewing a child so
    /// leaving the child restores the right activity state.
    pub root_active_turn: Option<TurnId>,
    /// Parent threads above the current view; non-empty = drilled into a
    /// child-agent thread. Up returns; Left/Right move between siblings.
    pub parent_stack: Vec<Thread>,
    /// Live child-agent progress by `task` call id.
    pub children: std::collections::HashMap<String, ChildTask>,
    /// Every unanswered permission prompt in this root conversation, oldest
    /// first — including prompts raised on child-agent threads.
    pub pending_permissions: Vec<PendingPermissionInfo>,
    /// Whether the open chat is in Auto. On a server with shared approval
    /// modes this mirrors the server's setting for the chat; on an older
    /// server it is this TUI launch's own switch. Never a permission rule.
    pub auto_permissions: bool,
    pub auto_permissions_seen: std::collections::HashMap<String, bool>,
    /// The server applies Auto itself, for every client.
    pub approval_server: bool,
    /// The server's Auto asks a reviewer model (`approval-review.v1`).
    pub approval_reviewer: bool,
    /// Launched with `--yolo`: this TUI answers every request its open
    /// conversation raises once, and its turns approve their own. The server
    /// is not a yolo server, so other clients' chats still ask.
    pub yolo: bool,
    pub approval_mode: Option<haarness_protocol::ApprovalModeInfo>,
    /// True while the approval card owns the keyboard: its hotkeys answer the
    /// first pending permission and the composer draws no cursor. Ctrl+G and
    /// any key that is not an answer hand the keyboard back to the composer.
    pub permission_focus: bool,
    /// A pending 'a'/'w'/'r'/'u' answer awaiting its confirmation press: the
    /// "always" rules, or Once + Auto, which changes how the whole chat is
    /// answered from then on.
    pub confirm_always: Option<char>,
    /// Letters the focused approval card took without answering (`h`/`l`
    /// moves and staged choices), with the chip selection from before them
    /// and when the last one was taken. When the next key shows they began a
    /// message, they move to the composer with it instead.
    pub permission_typed: Option<(String, usize, std::time::Instant)>,
    /// Highlighted chip on the docked approval panel: an index into
    /// `permission_choices`, or into confirm/cancel while `confirm_always`
    /// is staged.
    pub permission_selection: usize,
    /// The approval card's subject scroll, for the request it belongs to.
    pub permission_scroll: Option<PermissionScroll>,
    /// How that subject fitted the most recently rendered card.
    pub permission_fit: std::cell::Cell<SubjectFit>,
    /// What the current thread's running turn is doing (turn.status).
    pub phase: Option<PhaseInfo>,
    /// Every unanswered question in this root conversation, oldest first.
    /// Structured entries keep parallel child-agent prompts from overwriting
    /// each other and make reconnect hydration independent of transcript rows.
    pub pending_questions: Vec<PendingQuestionInfo>,
    /// Keyboard/mouse focus within the first pending question's choices.
    /// This is deliberately independent from overlay/autocomplete selection:
    /// an event may raise a question while a dialog is open, and neither
    /// surface should steal the other's highlighted row.
    pub question_selection: usize,
    /// Checked rows for the active multi-select question. A set keeps mouse,
    /// keyboard, and replay updates deterministic without changing the
    /// protocol's textual reply envelope.
    pub question_selected: std::collections::BTreeSet<usize>,
    pub selection: usize,
    pub overlay_filter: String,
    pub message_browser: Option<crate::messages::BrowserPosition>,
    pub pending_message_jump: std::cell::Cell<Option<(ThreadId, haarness_protocol::ItemId)>>,
    /// Scroll position owned by an informational modal. It is intentionally
    /// separate from transcript scrollback.
    pub overlay_scroll: usize,
    pub max_overlay_scroll: std::cell::Cell<usize>,
    /// `!` shell passthrough: Enter runs the composer as a command.
    pub shell_mode: bool,
    /// Full-screen terminal tabs. Dropping this view detaches the client but
    /// leaves server-owned PTY processes running.
    pub terminal: Option<crate::pty_view::TerminalWorkspace>,
    /// Live @-mention completion results from the server.
    pub mention_suggestions: Vec<String>,
    /// The attached server's MCP resources for the `@` list.
    pub(crate) mention_resources: crate::resource_mentions::Listing,
    /// Where a resource listing read off the event loop goes (see
    /// `App::resources_arrived`); without it, none is asked for.
    pub(crate) resources_tx: Option<tokio::sync::mpsc::Sender<crate::resource_mentions::Update>>,
    /// Where a message sent with mentions off the event loop reports back
    /// (see `App::mentions_sent`); without it, the send waits in place.
    pub(crate) sends_tx: Option<tokio::sync::mpsc::Sender<crate::resource_mentions::Sent>>,
    /// Escape dismisses autocomplete without deleting the typed query. The
    /// next edit makes suggestions eligible again.
    pub autocomplete_hidden: bool,
    /// Transcript scroll position, counted in lines from the bottom.
    pub viewport_offset: usize,
    /// Largest meaningful transcript offset for the most recently rendered
    /// viewport. Scroll input clamps to this instead of accumulating forever.
    pub max_viewport_offset: std::cell::Cell<usize>,
    /// Rows the transcript had in the most recently rendered frame; page keys
    /// move by about this much.
    pub transcript_rows: std::cell::Cell<usize>,
    /// Render-time correction to the saved bottom-relative offset while the
    /// user is scrolled up. This is signed because append-only Markdown can
    /// reflow to either more or fewer terminal rows as delimiters close.
    pub scroll_adjustment: std::cell::Cell<isize>,
    pub last_transcript_len: std::cell::Cell<usize>,
    /// The messages of the most recently rendered transcript and the row each
    /// starts on, top first: where the message keys step to.
    pub transcript_messages: std::cell::RefCell<Vec<(haarness_protocol::ItemId, usize)>>,
    /// Context estimate of the shown thread, keyed by its item count, last
    /// sequence and undo boundary; the meters read it every frame.
    pub context_tokens: std::cell::Cell<Option<ContextEstimate>>,
    /// Server identity (version, workspace, git branch) for the header/home.
    pub server_info: Option<haarness_protocol::ServerInfo>,
    /// Render tick (~5/s) driving the working spinner.
    pub tick: u64,
    /// When the active turn started, for the elapsed display.
    pub turn_started: Option<std::time::Instant>,
    /// First Escape arms the visible turn or root session; the same target
    /// must receive a second Escape within five seconds before interruption.
    pub escape_interrupt: Option<(InterruptTarget, std::time::Instant)>,
    /// First empty Ctrl+C arms turn/session interruption or app quit; the
    /// matching second press confirms it.
    pub ctrl_c_action: Option<(InterruptTarget, std::time::Instant)>,
    /// Effort sent with new turns: minimal|low|medium|high, None = provider
    /// default.
    pub reasoning_effort: Option<String>,
    pub theme: Theme,
    /// A saved or committed palette choice takes precedence over automatic
    /// terminal-background matching, even on ANSI-256 terminals.
    pub theme_selected: bool,
    /// Whether ordinary surfaces should reveal the terminal background.
    /// Overlays and selection colors stay opaque.
    pub terminal_background: bool,
    /// The theme in force before the picker opened. Present only while the
    /// picker is open: choosing a theme is a visual decision, so the
    /// highlighted row is applied live and Escape has to undo it.
    pub theme_preview_restore: Option<(String, String)>,
    pub theme_name: String,
    pub theme_variant: String,
    pub themes: Vec<crate::theme::ThemeEntry>,
    /// Tool results the user expanded to full output.
    pub expanded: std::collections::HashSet<ItemId>,
    /// Show successful read/list/search result bodies by default. Off keeps
    /// routine exploration compact; individual rows remain clickable.
    pub tool_details: bool,
    /// Right pane visibility where it docks beside the conversation (on by
    /// default; terminals narrower than that never dock it).
    pub sidebar_open: bool,
    /// The right pane opened over the conversation's right edge with ctrl+b
    /// on a terminal too narrow to dock it, as Upstream does.
    pub sidebar_overlay: bool,
    /// Width of the last rendered frame, so ctrl+b can tell a docked sidebar
    /// from one that has to open over the conversation.
    pub frame_width: std::cell::Cell<usize>,
    /// Whether the last frame left the sidebar off screen, so the transcript
    /// carries a one-row summary of the plan in its place.
    pub plan_inline: std::cell::Cell<bool>,
    /// Collapsed sidebar section indices.
    pub sidebar_collapsed: std::collections::HashSet<usize>,
    /// MCP status shown in the sidebar, refreshed when it opens.
    pub mcp_status: Vec<haarness_protocol::McpServerInfo>,
    /// User-defined slash commands served by the server.
    pub server_commands: Vec<haarness_protocol::CommandInfo>,
    /// The thread's durable goal-harness state, when set.
    pub goal: Option<haarness_protocol::GoalState>,
    /// Last authoritative goal snapshot for each visited thread. Switching
    /// can render this immediately while a fresh REST read is retried.
    pub goal_snapshots: std::collections::HashMap<ThreadId, Option<haarness_protocol::GoalState>>,
    /// Localized next-step labels attached to the most recent completion.
    pub goal_completion: Option<haarness_protocol::GoalCompletion>,
    /// Deduplicates replayed completion events after reconnect/resync.
    pub last_goal_completion: Option<String>,
    /// A transient goal snapshot read failed and should be retried without
    /// erasing the last authoritative state shown to the user.
    pub goal_refresh_needed: bool,
    /// Workflow catalog and recent durable runs backing the searchable
    /// dashboard. These are REST snapshots, never independently mutated UI
    /// state.
    pub workflow_definitions: Vec<WorkflowDefinition>,
    pub workflow_invalid: Vec<WorkflowInvalidDefinition>,
    pub workflow_runs: Vec<WorkflowRun>,
    /// Authoritative snapshot behind the workspace dashboard/actions. The
    /// selected ID and in-progress name only exist while moving through its
    /// modal flow; mutations always go back through the real REST API.
    pub workspace_catalog: Option<WorkspaceListResponse>,
    /// Only resolver-backed local clients can relaunch this process into a
    /// workspace path. Attached clients may create/reset/remove remotely but
    /// must not be offered an Open action they cannot complete.
    pub workspace_local_switching: bool,
    pub workspace_selected: Option<String>,
    pub workspace_create_name: Option<String>,
    /// Last successfully discovered ref catalog. `None` deliberately renders
    /// a fallback picker containing Default and Custom after a load failure.
    pub workspace_refs: Option<WorkspaceRefsResponse>,
    /// Exact arbitrary ref text retained across validation failures and back.
    pub workspace_custom_ref: String,
    /// Ref discovery/validation currently running on the background worker.
    pub workspace_ref_operation: Option<WorkspaceRefOperation>,
    /// Slow Git-backed mutation currently running on the event loop's worker
    /// task. While set, the progress dialog cannot be dismissed or duplicated.
    pub workspace_mutation: Option<WorkspaceMutation>,
    /// Review currently being fetched for the `/diff` viewer, on a worker
    /// task. Leaving the viewer cancels it; a frame collects it.
    pub(crate) diff_load: Option<crate::commands::DiffLoad>,
    /// OSC notification queued by the reducer, drained by the event loop.
    pub pending_notification: Option<String>,
    /// "Finished" banners waiting out their grace; see `state::completion`.
    pub completions: completion::HeldCompletions,
    /// Visible in-TUI companion for desktop notifications. This makes
    /// completion/approval feedback work even when a terminal ignores OSC 9.
    pub notification_toast: Option<Toast>,
    /// Whether the terminal window has focus (true until it says otherwise).
    /// Completion toasts and desktop notifications are for the reader who
    /// looked away; the transcript already tells the one who is watching.
    pub terminal_focused: bool,
    /// Cell-bounded click targets of the last rendered frame.
    pub hit_rows: std::cell::RefCell<Vec<crate::view::frame::HitRegion>>,
    /// Left-button press position awaiting its release (click detection).
    pub mouse_down: Option<(u16, u16)>,
    /// Screen cell last reached by a real mouse-movement event. Storing the
    /// cell rather than only the target keeps repeated targets (for example a
    /// task shown in transcript and sidebar) from highlighting together.
    pub hovered_cell: Option<(u16, u16)>,
    /// Screen-anchored drag selection: (anchor, head), char-cell coords.
    pub selection_range: Option<((u16, u16), (u16, u16))>,
    /// Plain text of the last rendered frame rows, for selection copy.
    pub last_frame: std::cell::RefCell<Vec<String>>,
    /// Rendered-lines cache for expensive items (markdown, diffs); entries
    /// are keyed by item and invalidated by width or generation changes.
    pub render_cache: std::cell::RefCell<std::collections::HashMap<ItemId, CachedRender>>,
    /// Files and lines an edit result changed, keyed by the result item.
    /// Counting means diffing its bodies, too slow to repeat every frame.
    pub diff_stats: std::cell::RefCell<std::collections::HashMap<ItemId, (usize, u64, u64)>>,
    /// Each batch result's entries as (tool, failed), keyed by the result
    /// item. Its metadata holds every step's output, too much to walk or
    /// parse every frame.
    pub batch_results: std::cell::RefCell<std::collections::HashMap<ItemId, BatchOutcomes>>,
    /// Bumped when rendering inputs beyond width change (theme switches).
    pub render_generation: u64,
}

pub struct CachedRender {
    pub width: usize,
    pub generation: u64,
    pub lines: Vec<haarness_opentui::Line>,
}

impl App {
    pub(crate) fn sync_prompt_history(&mut self, client: &crate::api::ApiClient) {
        let workspace = self
            .server_info
            .as_ref()
            .and_then(|info| info.workspace.as_deref())
            .unwrap_or("");
        let scope = if client.supports_local_workspace_switch() {
            format!("local:{workspace}")
        } else {
            format!("{}:{workspace}", client.server_url())
        };
        let thread = self.thread.id.to_string();
        if self
            .composer
            .history_owner()
            .is_some_and(|owner| owner.0 == scope && owner.1 == thread)
        {
            return;
        }
        let history = crate::config::load_scoped_history(&scope, &thread);
        self.composer.set_history_owner(scope, thread, history);
    }
    pub(crate) fn save_prompt_history(&mut self) {
        let Some(text) = self.composer.pending_history() else {
            return;
        };
        let Some((scope, _)) = self.composer.history_owner() else {
            return;
        };
        if let Err(error) =
            crate::config::save_scoped_history(scope, &self.thread.id.to_string(), &text)
        {
            self.notice = format!("Prompt sent; history could not be saved: {error}");
            return;
        }
        self.keep_picks(&[text]);
    }

    pub fn new(
        thread: Thread,
        threads: Vec<Thread>,
        items: Vec<Item>,
        providers: Vec<ProviderInfo>,
        agents: Vec<AgentProfile>,
    ) -> Self {
        let (keybindings, keybindings_skipped) = crate::keybindings::load();
        let agent_index = agents
            .iter()
            .position(|agent| agent.id == thread.agent)
            .unwrap_or(0);
        let notice = if providers
            .iter()
            .any(|provider| provider.id == thread.provider && provider.authenticated)
        {
            "Connected".into()
        } else {
            unconnected_notice(&providers, &thread.provider)
        };
        let root_thread_id = compute_root(&threads, thread.id);
        let children = durable_children(&items);
        let todos = durable_todos(&items, thread.revert.as_ref().map(|state| state.sequence));
        Self {
            root_thread_id,
            thread,
            threads,
            session_include_archived: false,
            items,
            todos,
            providers,
            agents,
            agent_index,
            composer: Composer::default(),
            drafts: std::collections::HashMap::new(),
            draft_baseline: std::cell::RefCell::default(),
            attachments: Vec::new(),
            preview: String::new(),
            reasoning_preview: String::new(),
            live_threads: std::collections::HashMap::new(),
            waiting: std::collections::HashMap::new(),
            notice,
            notice_shown: None,
            undo_draft: None,
            custom_status_line: None,
            billing: haarness_account::billing::State::default(),
            language: crate::locale::Language::default(),
            connection_lost: None,
            models_loading: false,
            catalog_tx: None,
            lost_toast_at: None,
            following_stopped: false,
            auto_model: None,
            server_restart: None,
            overlay: Overlay::None,
            thinking: ThinkingMode::default(),
            active_turn: None,
            refresh_needed: false,
            input_queue: haarness_protocol::InputQueue::default(),
            input_queue_edit: None,
            input_queue_listed: false,
            queue_edit: None,
            root_active_turn: None,
            parent_stack: Vec::new(),
            children,
            pending_permissions: Vec::new(),
            auto_permissions: false,
            approval_reviewer: false,
            auto_permissions_seen: std::collections::HashMap::new(),
            approval_server: false,
            yolo: false,
            approval_mode: None,
            permission_focus: false,
            confirm_always: None,
            permission_typed: None,
            permission_selection: 0,
            permission_scroll: None,
            permission_fit: std::cell::Cell::new(SubjectFit::default()),
            phase: None,
            pending_questions: Vec::new(),
            question_selection: 0,
            question_selected: std::collections::BTreeSet::new(),
            selection: 0,
            overlay_filter: String::new(),
            message_browser: None,
            pending_message_jump: std::cell::Cell::new(None),
            overlay_scroll: 0,
            max_overlay_scroll: std::cell::Cell::new(0),
            shell_mode: false,
            terminal: None,
            mention_suggestions: Vec::new(),
            mention_resources: crate::resource_mentions::Listing::default(),
            resources_tx: None,
            sends_tx: None,
            autocomplete_hidden: false,
            viewport_offset: 0,
            max_viewport_offset: std::cell::Cell::new(0),
            transcript_rows: std::cell::Cell::new(0),
            scroll_adjustment: std::cell::Cell::new(0),
            last_transcript_len: std::cell::Cell::new(0),
            transcript_messages: std::cell::RefCell::new(Vec::new()),
            context_tokens: std::cell::Cell::new(None),
            server_info: None,
            tick: 0,
            turn_started: None,
            escape_interrupt: None,
            ctrl_c_action: None,
            reasoning_effort: None,
            management: crate::management::Management::default(),
            connect: crate::connect::ConnectFlow::default(),
            remote: crate::remote_control::RemoteControl::default(),
            chrome: crate::chrome::Chrome::default(),
            logs: crate::logs::Logs::default(),
            side: crate::side_chat::SideChats::default(),
            pending_server: None,
            original_client: None,
            keybindings,
            keybindings_skipped,
            model_favorites: crate::config::load_state().model_favorites,
            model_recents: crate::config::load_state().model_recents,
            theme: Theme::default(),
            theme_selected: false,
            terminal_background: false,
            theme_name: crate::theme::DEFAULT_THEME.into(),
            theme_variant: "dark".into(),
            themes: crate::theme::builtin_entries(),
            expanded: std::collections::HashSet::new(),
            tool_details: false,
            sidebar_open: true,
            sidebar_overlay: false,
            frame_width: std::cell::Cell::new(0),
            plan_inline: std::cell::Cell::new(false),
            sidebar_collapsed: std::collections::HashSet::new(),
            mcp_status: Vec::new(),
            server_commands: Vec::new(),
            goal: None,
            goal_snapshots: std::collections::HashMap::new(),
            goal_completion: None,
            last_goal_completion: None,
            goal_refresh_needed: false,
            workflow_definitions: Vec::new(),
            workflow_invalid: Vec::new(),
            workflow_runs: Vec::new(),
            workspace_catalog: None,
            // App construction alone does not prove that this process can
            // relaunch into another local directory. run_with_client sets the
            // capability from the resolver-backed ApiClient at startup.
            workspace_local_switching: false,
            workspace_selected: None,
            workspace_create_name: None,
            workspace_refs: None,
            workspace_custom_ref: String::new(),
            workspace_ref_operation: None,
            workspace_mutation: None,
            diff_load: None,
            pending_notification: None,
            completions: completion::HeldCompletions::default(),
            notification_toast: None,
            terminal_focused: true,
            theme_preview_restore: None,
            hit_rows: std::cell::RefCell::new(Vec::new()),
            mouse_down: None,
            hovered_cell: None,
            selection_range: None,
            last_frame: std::cell::RefCell::new(Vec::new()),
            render_cache: std::cell::RefCell::new(std::collections::HashMap::new()),
            diff_stats: std::cell::RefCell::new(std::collections::HashMap::new()),
            batch_results: std::cell::RefCell::new(std::collections::HashMap::new()),
            render_generation: 0,
        }
    }

    /// Merge durable task links from the current root transcript without
    /// overwriting richer live progress already received over SSE.
    pub fn hydrate_child_links(&mut self) {
        for (call_id, task) in durable_children(&self.items) {
            match self.children.entry(call_id) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(task);
                }
                std::collections::hash_map::Entry::Occupied(mut entry)
                    if matches!(
                        task.state.as_str(),
                        "background" | "done" | "failed" | "cancelled"
                    ) =>
                {
                    // A paired durable task result is authoritative after an
                    // SSE replay gap. Preserve richer live activity metadata,
                    // but never leave the lifecycle stuck on running.
                    entry.get_mut().state = task.state;
                }
                std::collections::hash_map::Entry::Occupied(_) => {}
            }
        }
    }

    /// Rows relevant to the current interaction: children invoked by the
    /// latest visible turn plus older children that are still active. Durable
    /// completed children remain in `threads` and `items` for transcript and
    /// navigation history without permanently occupying the live sidebar.
    pub fn visible_children(&self) -> Vec<(&String, &ChildTask)> {
        // A late background checkpoint keeps the launch turn's ID even when
        // it is appended after a newer user request. Anchor the roster to the
        // latest request, not simply the last appended item, so completion of
        // an old child cannot resurrect an old completed roster.
        let latest_turn_id = self
            .items
            .iter()
            .rev()
            .find_map(|item| {
                matches!(
                    &item.data,
                    ItemData::UserMessage {
                        synthetic: false,
                        ..
                    }
                )
                .then_some(item.turn_id)
                .flatten()
            })
            .or_else(|| self.items.iter().rev().find_map(|item| item.turn_id));
        self.children
            .iter()
            .filter(|(_, task)| {
                task.is_active()
                    || latest_turn_id.is_none()
                    || task.invoked_turn_id == latest_turn_id
            })
            .collect()
    }

    pub(crate) fn child_row_for_thread(&self, child_thread_id: ThreadId) -> Option<String> {
        self.children.iter().find_map(|(call_id, task)| {
            (task.child_thread_id == child_thread_id).then(|| call_id.clone())
        })
    }

    pub(crate) fn child_thread_for_call(&self, call_id: &str) -> Option<ThreadId> {
        self.items.iter().rev().find_map(|item| match &item.data {
            ItemData::TaskLink {
                call_id: candidate,
                child_thread_id,
            } if candidate == call_id => Some(*child_thread_id),
            _ => None,
        })
    }

    /// Reconcile the client-side activity cache with the server's complete
    /// process-wide snapshot. This is intentionally independent of the
    /// selected conversation: switching away is a view change, not task
    /// cancellation, and returning must immediately show the live turn.
    pub fn hydrate_active_turns(&mut self, turns: Vec<Turn>) {
        let active_threads = turns
            .iter()
            .map(|turn| turn.thread_id)
            .collect::<std::collections::HashSet<_>>();
        self.live_threads
            .retain(|thread_id, _| active_threads.contains(thread_id));
        for turn in &turns {
            self.live_threads.entry(turn.thread_id).or_default();
        }
        self.active_turn = turns
            .iter()
            .find(|turn| turn.thread_id == self.thread.id)
            .map(|turn| turn.id);
        self.root_active_turn = turns
            .iter()
            .find(|turn| turn.thread_id == self.root_thread_id)
            .map(|turn| turn.id);
        self.turn_started = self.active_turn.map(|_| std::time::Instant::now());
    }

    /// Active turns below the root currently being viewed. The root's own
    /// foreground turn is tracked separately so cancellation never targets an
    /// arbitrary child merely because detached work is still draining.
    pub fn root_descendant_active_count(&self) -> usize {
        self.live_threads
            .keys()
            .filter(|thread_id| {
                **thread_id != self.root_thread_id
                    && compute_root(&self.threads, **thread_id) == self.root_thread_id
            })
            .count()
    }

    /// Delegated threads are inspection surfaces. Forks also have a parent
    /// for history lineage, but remain editable conversations.
    pub fn is_child_view(&self) -> bool {
        self.thread.is_subagent
    }

    /// A launch or `/new` session nobody has written anything in yet. Leaving
    /// one behind would add another "New chat" to every session list, so
    /// it is archived on the way out instead.
    pub fn is_untouched_session(&self) -> bool {
        self.items.is_empty()
            && self.input_queue.items.is_empty()
            && self.composer.is_empty()
            && self.attachments.is_empty()
            && self.active_turn.is_none()
            && self.goal.is_none()
            && haarness_protocol::is_untitled_chat_title(&self.thread.title)
            && !self.thread.is_subagent
            && !self.thread.archived
            && self.thread.parent_id.is_none()
            && self.thread.fork_origin.is_none()
            && self.thread.schedule.is_none()
    }

    fn active_tree_turn_count(&self, root: ThreadId) -> usize {
        self.live_threads
            .keys()
            .filter(|thread_id| compute_root(&self.threads, **thread_id) == root)
            .count()
    }

    /// Transcript fallback used when an older server lacks the dedicated
    /// todos endpoint or a refresh is transiently unavailable.
    pub fn hydrate_todos_from_items(&mut self) {
        self.todos = durable_todos(
            &self.items,
            self.thread.revert.as_ref().map(|state| state.sequence),
        );
    }

    /// The connection to the server dropped: the status line says so at
    /// once; the toast at most every 30 s, so a proxy that drops it over and
    /// over cannot make it flicker.
    pub(crate) fn connection_dropped(&mut self, attempt: u32) {
        let first = self.connection_lost.is_none();
        self.connection_lost = Some(attempt);
        if first
            && self
                .lost_toast_at
                .is_none_or(|shown| shown.elapsed() >= std::time::Duration::from_secs(30))
        {
            self.lost_toast_at = Some(std::time::Instant::now());
            self.show_toast(
                None,
                "Connection lost · retrying".into(),
                ToastVariant::Warning,
                std::time::Duration::from_secs(4),
            );
        }
    }

    /// The server answers again.
    pub(crate) fn reconnected(&mut self) {
        self.connection_lost = None;
        self.following_stopped = false;
        self.notice = if self.server_restart.take() == Some(ServerRestart::Starting) {
            "Server restarted · reconnected"
        } else {
            "Reconnected"
        }
        .into();
    }

    /// The task following the server's events kept ending by itself: the
    /// status line says nothing is retrying, not "reconnecting".
    pub(crate) fn stopped_following(&mut self) {
        self.connection_lost = None;
        self.following_stopped = true;
    }

    pub fn show_toast(
        &mut self,
        title: Option<String>,
        message: String,
        variant: ToastVariant,
        duration: std::time::Duration,
    ) {
        self.notification_toast = Some(Toast {
            title,
            message,
            variant,
            expires_at: std::time::Instant::now() + duration,
            approval: false,
        });
    }

    /// Whether the composer still holds the prompt `/undo` put there,
    /// unedited. It is read from the chat, not remembered, so the prompt is
    /// still known after leaving the chat and coming back to its draft. It is
    /// compared as sent, references expanded, since that is what the chat
    /// keeps.
    pub(crate) fn holds_undo_draft(&self) -> bool {
        self.undone_prompt().is_some_and(|(text, attachments)| {
            self.composer.expanded_text() == text && self.attachments == attachments
        })
    }

    /// Put an undone prompt that a slash command was typed over back in an
    /// empty composer, once that command has run or been erased.
    pub(crate) fn return_undo_draft(&mut self) {
        if !self.composer.is_empty() || !self.attachments.is_empty() {
            return;
        }
        if let Some(draft) = self.undo_draft.take() {
            self.composer.set_text(draft.text);
            self.attachments = draft.attachments;
        }
    }

    /// Age the status-line notice. Notices are set from many places, so the
    /// render tick notes when the text changes and clears it once it has
    /// been on screen for [`NOTICE_TTL`]. A notice that describes a state
    /// still in force stays: an approval waiting, shell mode, a queued
    /// message being edited, a folder operation in flight.
    pub fn expire_notice(&mut self, now: std::time::Instant) {
        if self.notice.is_empty() {
            self.notice_shown = None;
            return;
        }
        match &self.notice_shown {
            Some((text, since)) if *text == self.notice => {
                let holds = !self.pending_permissions.is_empty()
                    || self.shell_mode
                    || self.queue_edit.is_some()
                    || self.workspace_mutation.is_some()
                    || self.workspace_ref_operation.is_some();
                if !holds && now.duration_since(*since) >= NOTICE_TTL {
                    self.notice.clear();
                    self.notice_shown = None;
                }
            }
            _ => self.notice_shown = Some((self.notice.clone(), now)),
        }
    }

    /// Whether the notice still answers something that happened a moment
    /// ago. Ambient news, such as the configuration reload a setting change
    /// causes, must not replace it.
    pub(crate) fn notice_is_fresh(&self, now: std::time::Instant) -> bool {
        !self.notice.trim().is_empty()
            && self.notice_shown.as_ref().is_none_or(|(text, since)| {
                *text != self.notice || now.duration_since(*since) < FRESH_NOTICE
            })
    }

    pub fn expire_notification(&mut self) {
        if self
            .notification_toast
            .as_ref()
            .is_some_and(|toast| std::time::Instant::now() >= toast.expires_at)
        {
            self.notification_toast = None;
        }
    }

    /// Remove an attention notification once the condition that produced it
    /// has been resolved. Matching the complete message avoids dismissing a
    /// newer sibling approval/question that happens to be pending at the same
    /// time.
    pub fn dismiss_notification(&mut self, message: &str) {
        if self
            .pending_notification
            .as_deref()
            .is_some_and(|pending| pending == message)
        {
            self.pending_notification = None;
        }
        if self
            .notification_toast
            .as_ref()
            .is_some_and(|toast| toast.message == message)
        {
            self.notification_toast = None;
        }
    }
    /// Every selectable theme as (name, variant, palette).
    pub fn theme_variants(&self) -> Vec<(String, String, Theme)> {
        self.themes
            .iter()
            .flat_map(|entry| {
                entry
                    .variants()
                    .into_iter()
                    .map(|(variant, theme)| (entry.name.clone(), variant.to_owned(), theme))
            })
            .collect()
    }
    pub fn filtered_options(&self, entries: &[OptionEntry]) -> Vec<OptionEntry> {
        // Creation prompts reuse the selector card's bounded text field, but
        // their sole Continue/Create row is an action rather than a search
        // result. Keep it clickable and Enter-able for every typed value.
        if matches!(
            self.overlay,
            Overlay::Options {
                kind: OptionsKind::ManagementField
                    | OptionsKind::ConnectApiKey
                    | OptionsKind::WorkspaceCreateName
                    | OptionsKind::WorkspaceCreateCustomBase,
                ..
            }
        ) {
            return entries.to_vec();
        }
        let normalize = |text: &str| {
            if matches!(
                self.overlay,
                Overlay::Options {
                    kind: OptionsKind::Messages,
                    ..
                }
            ) {
                text.to_lowercase()
            } else {
                text.to_ascii_lowercase()
            }
        };
        let query = normalize(&self.overlay_filter);
        entries
            .iter()
            .filter(|entry| {
                query.is_empty()
                    || normalize(&entry.label).contains(&query)
                    || normalize(&entry.description).contains(&query)
            })
            .cloned()
            .collect()
    }

    /// Replace the workflow dashboard with one authoritative catalog/run
    /// snapshot. Recent runs stay above reusable definitions so activity is
    /// immediately visible without hiding the things the user can launch.
    pub fn open_workflow_dashboard(
        &mut self,
        catalog: haarness_protocol::WorkflowListResponse,
        mut runs: Vec<WorkflowRun>,
    ) {
        // Keep the catalog directory-wide, but make the run
        // launched by the task the user is currently inspecting impossible to
        // miss. Stable origin priority preserves newest-first order inside
        // each scope.
        let current_thread = self.thread.id;
        let current_root = self.root_thread_id;
        runs.sort_by(|left, right| {
            workflow_origin_priority(left, current_thread, current_root)
                .cmp(&workflow_origin_priority(
                    right,
                    current_thread,
                    current_root,
                ))
                .then_with(|| right.time.updated.cmp(&left.time.updated))
        });
        self.workflow_runs = runs;
        self.workflow_definitions = catalog.workflows;
        self.workflow_definitions
            .sort_by(|left, right| left.invocation.cmp(&right.invocation));
        self.workflow_invalid = catalog.invalid;

        let mut entries = Vec::with_capacity(
            self.workflow_runs.len()
                + self.workflow_definitions.len()
                + self.workflow_invalid.len(),
        );
        for run in &self.workflow_runs {
            let mut detail = vec![
                "recent run".to_owned(),
                workflow_status(run.status).to_owned(),
            ];
            if let Some(phase) = run.phase.as_deref() {
                detail.push(phase.to_owned());
            }
            // How long it took is the first thing you want from a run list;
            // "updated 4m ago" cannot answer it.
            detail.push(crate::view::workflow::elapsed(
                run.time.started.unwrap_or(run.time.created),
                run.time.completed,
            ));
            let launch_id = run.definition.launch_thread_id.or(run.thread_id);
            detail.push(if launch_id == Some(self.thread.id) {
                "this task".to_owned()
            } else if launch_id == Some(self.root_thread_id) {
                "this conversation".to_owned()
            } else if let Some(thread) =
                launch_id.and_then(|id| self.threads.iter().find(|thread| thread.id == id))
            {
                format!("from {}", thread.title)
            } else if let Some(id) = launch_id {
                format!("from #{}", crate::view::span::short_id(&id.to_string()))
            } else {
                "directory run".to_owned()
            });
            detail.push(relative_time(run.time.updated));
            detail.push(crate::view::span::short_id(&run.id.to_string()).to_owned());
            entries.push(OptionEntry {
                label: format!(
                    "{}  {}",
                    workflow_status_marker(run.status),
                    run.workflow_name
                ),
                description: detail.join(" · "),
                value: format!("run:{}", run.id),
            });
        }
        for definition in &self.workflow_definitions {
            let mut detail = vec![
                "workflow".to_owned(),
                match definition.dialect {
                    haarness_protocol::WorkflowDialect::Native => "native".to_owned(),
                    haarness_protocol::WorkflowDialect::ClaudePortable => {
                        "claude portable".to_owned()
                    }
                },
            ];
            if let Some(plugin) = definition.plugin_name.as_deref() {
                detail.push(plugin.to_owned());
            }
            if let Some(description) = definition.meta.description.as_deref() {
                detail.push(description.to_owned());
            }
            entries.push(OptionEntry {
                label: format!("◇  {}", definition.invocation),
                description: detail.join(" · "),
                value: format!("definition:{}", definition.invocation),
            });
        }
        for (index, invalid) in self.workflow_invalid.iter().enumerate() {
            let filename = std::path::Path::new(&invalid.path).file_name().map_or_else(
                || invalid.path.as_str().into(),
                |name| name.to_string_lossy(),
            );
            entries.push(OptionEntry {
                label: format!("⚠  {filename}"),
                description: format!("invalid definition · {}", invalid.error),
                value: format!("invalid:{index}"),
            });
        }
        if entries.is_empty() {
            entries.push(OptionEntry {
                label: "No workflows yet".into(),
                description: "Add one in .haarness/workflows/<name>.ts".into(),
                value: "empty".into(),
            });
        }
        self.open_overlay(Overlay::Options {
            title: "Workflows".into(),
            kind: OptionsKind::WorkflowDashboard,
            entries,
        });
    }

    /// Insert an action result into the cached recent list without allowing a
    /// stale duplicate to remain visible.
    pub fn upsert_workflow_run(&mut self, run: WorkflowRun) {
        if let Some(existing) = self
            .workflow_runs
            .iter_mut()
            .find(|candidate| candidate.id == run.id)
        {
            *existing = run;
        } else {
            self.workflow_runs.push(run);
        }
        self.workflow_runs
            .sort_by(|left, right| right.time.updated.cmp(&left.time.updated));
    }

    /// Rebuild an already-open dashboard from SSE-updated snapshots while
    /// keeping its search and logical selection stable. Closed overlays are
    /// deliberately untouched; workflow events never steal focus.
    pub fn refresh_open_workflow_dashboard(&mut self) {
        if !matches!(
            self.overlay,
            Overlay::Options {
                kind: OptionsKind::WorkflowDashboard,
                ..
            }
        ) {
            return;
        }
        let selected_value = match self.selected_overlay_choice() {
            Some(OverlayChoice::Option(OptionsKind::WorkflowDashboard, value)) => Some(value),
            _ => None,
        };
        let filter = self.overlay_filter.clone();
        let catalog = haarness_protocol::WorkflowListResponse {
            workflows: self.workflow_definitions.clone(),
            invalid: self.workflow_invalid.clone(),
        };
        let runs = self.workflow_runs.clone();
        self.open_workflow_dashboard(catalog, runs);
        self.overlay_filter = filter;
        let selection = if let Overlay::Options { ref entries, .. } = self.overlay {
            selected_value.and_then(|value| {
                self.filtered_options(entries)
                    .iter()
                    .position(|entry| entry.value == value)
            })
        } else {
            None
        };
        self.selection = selection
            .unwrap_or(self.selection)
            .min(self.current_selection_length().saturating_sub(1));
    }

    /// Hand the inspected run to its lifecycle controls. The snapshot behind
    /// the inspector is already cached, so this never needs the network.
    pub fn open_cached_workflow_run_actions(&mut self, id: haarness_protocol::WorkflowId) {
        if let Some(run) = self
            .workflow_runs
            .iter()
            .find(|candidate| candidate.id == id)
            .cloned()
        {
            self.open_workflow_run_actions(&run);
        }
    }

    pub fn open_workflow_run_actions(&mut self, run: &WorkflowRun) {
        let id = run.id.to_string();
        let mut entries = vec![OptionEntry {
            label: "Inspect run".into(),
            description: "Status, phases, agents, warnings, logs, and result".into(),
            value: format!("inspect:{id}"),
        }];
        if run.thread_id.is_some() {
            entries.push(OptionEntry {
                label: "Open workflow session".into(),
                description: "Inspect the dedicated child conversation and tool activity".into(),
                value: format!("session:{id}"),
            });
        }
        if let Some(launch_id) = run.definition.launch_thread_id
            && launch_id != self.thread.id
        {
            entries.push(OptionEntry {
                label: "Open launching task".into(),
                description: self
                    .threads
                    .iter()
                    .find(|thread| thread.id == launch_id)
                    .map_or_else(
                        || {
                            format!(
                                "Conversation #{}",
                                crate::view::span::short_id(&launch_id.to_string())
                            )
                        },
                        |thread| format!("Conversation that launched this run · {}", thread.title),
                    ),
                value: format!("launch:{id}"),
            });
        }
        match run.status {
            WorkflowRunStatus::Running => {
                entries.push(OptionEntry {
                    label: "Pause".into(),
                    description: "Stop this execution; resume restarts it from the beginning"
                        .into(),
                    value: format!("pause:{id}"),
                });
                entries.push(OptionEntry {
                    label: "Cancel".into(),
                    description: "Stop this run without deleting its history".into(),
                    value: format!("cancel:{id}"),
                });
            }
            WorkflowRunStatus::Paused | WorkflowRunStatus::Interrupted => {
                entries.push(OptionEntry {
                    label: "Resume".into(),
                    description: "Restart from the immutable source snapshot".into(),
                    value: format!("resume:{id}"),
                });
                if run.status == WorkflowRunStatus::Paused {
                    entries.push(OptionEntry {
                        label: "Cancel".into(),
                        description: "Finish this paused run without deleting its history".into(),
                        value: format!("cancel:{id}"),
                    });
                }
            }
            WorkflowRunStatus::Completed
            | WorkflowRunStatus::Failed
            | WorkflowRunStatus::Cancelled => {}
        }
        entries.extend([
            OptionEntry {
                label: "Copy run ID".into(),
                description: id.clone(),
                value: format!("copy:{id}"),
            },
            OptionEntry {
                label: "Delete run".into(),
                description: "Permanently remove this run and its durable logs".into(),
                value: format!("delete:{id}"),
            },
        ]);
        self.open_overlay(Overlay::Options {
            title: format!("{} · {}", run.workflow_name, workflow_status(run.status)),
            kind: OptionsKind::WorkflowRunActions,
            entries,
        });
    }

    /// Open the live inspector for `run`. The snapshot is cached first so the
    /// dialog keeps rendering — and keeps updating — from whatever the server
    /// last told us, rather than from a copy frozen at open time.
    pub fn open_workflow_run_details(&mut self, run: &WorkflowRun) {
        self.upsert_workflow_run(run.clone());
        self.open_overlay(Overlay::Workflow {
            target: WorkflowTarget::Run(run.id),
        });
    }

    /// Inspect whatever the dashboard has selected without acting on it.
    /// Starting a workflow you have not read is the mistake this prevents:
    /// the definition view is where its arguments are documented.
    pub fn inspect_selected_workflow(&mut self) {
        let Overlay::Options {
            kind: OptionsKind::WorkflowDashboard,
            ref entries,
            ..
        } = self.overlay
        else {
            return;
        };
        let options = self.filtered_options(entries);
        let Some(value) = selected(&options, self.selection).map(|entry| entry.value.clone())
        else {
            return;
        };
        let Some((kind, target)) = value.split_once(':') else {
            return;
        };
        let target = match kind {
            "run" => target
                .parse::<uuid::Uuid>()
                .ok()
                .map(|id| WorkflowTarget::Run(haarness_protocol::WorkflowId(id))),
            "definition" => Some(WorkflowTarget::Definition(target.to_owned())),
            "invalid" => target.parse::<usize>().ok().map(WorkflowTarget::Invalid),
            _ => None,
        };
        if let Some(target) = target {
            self.open_overlay(Overlay::Workflow { target });
        }
    }
    pub fn filtered_themes(&self) -> Vec<(String, String, Theme)> {
        let query = self.overlay_filter.to_ascii_lowercase();
        self.theme_variants()
            .into_iter()
            .filter(|(name, variant, _)| {
                query.is_empty()
                    || name.to_ascii_lowercase().contains(&query)
                    || variant.contains(&query)
            })
            .collect()
    }
    /// Apply a theme variant; false when unknown.
    pub fn apply_theme(&mut self, name: &str, variant: &str) -> bool {
        // Restore the renamed built-in palette without overriding a custom theme.
        let name = if name == "opencode" && !self.themes.iter().any(|entry| entry.name == name) {
            "classic"
        } else {
            name
        };
        let Some((_, _, theme)) =
            self.theme_variants()
                .into_iter()
                .find(|(candidate, candidate_variant, _)| {
                    candidate == name && candidate_variant == variant
                })
        else {
            return false;
        };
        self.theme = if self.terminal_background {
            theme.using_terminal_background()
        } else {
            theme
        };
        name.clone_into(&mut self.theme_name);
        variant.clone_into(&mut self.theme_variant);
        self.invalidate_renders();
        true
    }

    /// Re-resolve the current theme whenever the renderer's background mode
    /// changes. Keeping the source palettes untouched makes theme previews
    /// and later switches apply the same transformation consistently.
    pub fn set_terminal_background(&mut self, enabled: bool) {
        if self.terminal_background == enabled {
            return;
        }
        self.terminal_background = enabled;
        let name = self.theme_name.clone();
        let variant = self.theme_variant.clone();
        let _ = self.apply_theme(&name, &variant);
    }
    /// The theme the picker would return to on Escape. Rows are marked
    /// against this, not against the previewed theme, so the dot keeps
    /// pointing at what you already had.
    #[must_use]
    pub fn theme_baseline(&self) -> (&str, &str) {
        self.theme_preview_restore.as_ref().map_or(
            (self.theme_name.as_str(), self.theme_variant.as_str()),
            |(name, variant)| (name.as_str(), variant.as_str()),
        )
    }

    /// Apply the highlighted row while the theme picker is open. A list of
    /// names cannot tell you what a palette looks like; the interface itself
    /// is the swatch.
    pub fn preview_selected_theme(&mut self) {
        if !matches!(self.overlay, Overlay::Themes) {
            return;
        }
        let Some((name, variant, _)) = selected(&self.filtered_themes(), self.selection).cloned()
        else {
            return;
        };
        if name == self.theme_name && variant == self.theme_variant {
            return;
        }
        self.apply_theme(&name, &variant);
    }

    /// Invalidate cached renders (theme switches, style-affecting toggles).
    pub fn invalidate_renders(&mut self) {
        self.render_generation += 1;
    }
    /// Cached per-item rendering: rebuilds when the width or generation moved.
    pub fn cached_render(
        &self,
        id: ItemId,
        width: usize,
        build: impl FnOnce() -> Vec<haarness_opentui::Line>,
    ) -> Vec<haarness_opentui::Line> {
        let mut cache = self.render_cache.borrow_mut();
        if let Some(entry) = cache.get(&id) {
            if entry.width == width && entry.generation == self.render_generation {
                return entry.lines.clone();
            }
        }
        let lines = build();
        cache.insert(
            id,
            CachedRender {
                width,
                generation: self.render_generation,
                lines: lines.clone(),
            },
        );
        lines
    }
    /// An item's diff stat, counted once: results never change after they land.
    pub fn cached_diff_stat(
        &self,
        id: ItemId,
        count: impl FnOnce() -> (usize, u64, u64),
    ) -> (usize, u64, u64) {
        *self.diff_stats.borrow_mut().entry(id).or_insert_with(count)
    }
    /// A batch result's entries, read once: results never change after they land.
    pub fn cached_batch_results(
        &self,
        id: ItemId,
        read: impl FnOnce() -> Vec<BatchOutcome>,
    ) -> BatchOutcomes {
        self.batch_results
            .borrow_mut()
            .entry(id)
            .or_insert_with(|| read().into())
            .clone()
    }
    /// Scroll the transcript; positive steps move toward older content. The
    /// view clamps against the actual transcript height.
    pub fn scroll(&mut self, step: isize) {
        self.selection_range = None;
        let current = self.effective_viewport_offset();
        self.scroll_adjustment.set(0);
        if step >= 0 {
            self.viewport_offset = current
                .saturating_add(step.unsigned_abs())
                .min(self.max_viewport_offset.get());
        } else {
            self.viewport_offset = current.saturating_sub(step.unsigned_abs());
        }
    }
    /// One page for PgUp/PgDn: the transcript's visible rows less a little
    /// overlap, so the line read last stays in view.
    pub fn page_rows(&self) -> isize {
        const OVERLAP: usize = 2;
        match self.transcript_rows.get() {
            // Nothing rendered yet.
            0 => 10,
            rows => isize::try_from(rows.saturating_sub(OVERLAP).max(1)).unwrap_or(isize::MAX),
        }
    }
    pub fn scroll_to_bottom(&mut self) {
        self.viewport_offset = 0;
        self.scroll_adjustment.set(0);
    }
    pub fn effective_viewport_offset(&self) -> usize {
        let adjustment = self.scroll_adjustment.get();
        if adjustment >= 0 {
            self.viewport_offset
                .saturating_add(adjustment.unsigned_abs())
        } else {
            self.viewport_offset
                .saturating_sub(adjustment.unsigned_abs())
        }
    }
    pub(crate) fn set_effective_viewport_offset(&self, offset: usize) {
        let adjustment = if offset >= self.viewport_offset {
            isize::try_from(offset - self.viewport_offset).unwrap_or(isize::MAX)
        } else {
            -isize::try_from(self.viewport_offset - offset).unwrap_or(isize::MAX)
        };
        self.scroll_adjustment.set(adjustment);
    }
    /// Choice text for the pending question, when it offered numbered choices.
    pub fn question_choice(&self, index: usize) -> Option<String> {
        self.pending_questions
            .first()
            .and_then(|pending| pending.choices.get(index))
            .cloned()
    }

    pub fn toggle_question_choice(&mut self, index: usize) {
        let valid = self
            .pending_questions
            .first()
            .is_some_and(|pending| pending.multiple && index < pending.choices.len());
        if !valid {
            return;
        }
        if !self.question_selected.remove(&index) {
            self.question_selected.insert(index);
        }
    }

    /// Move focus through the active question's choices, wrapping at both ends.
    pub fn move_question_selection(&mut self, direction: isize) {
        let length = self
            .pending_questions
            .first()
            .map_or(0, |pending| pending.choices.len());
        if length == 0 {
            self.question_selection = 0;
            return;
        }

        let current = self.question_selection.min(length - 1);
        if direction < 0 {
            let distance = direction.unsigned_abs() % length;
            self.question_selection = (current + length - distance) % length;
        } else {
            self.question_selection = (current + direction.cast_unsigned() % length) % length;
        }
    }

    pub fn first_pending_question(&self) -> Option<&PendingQuestionInfo> {
        self.pending_questions.first()
    }

    pub fn queue_question(&mut self, question: PendingQuestionInfo) {
        if self
            .pending_questions
            .iter()
            .any(|pending| pending.question_id == question.question_id)
        {
            return;
        }
        let previous_first = self
            .pending_questions
            .first()
            .map(|pending| pending.question_id.clone());
        self.pending_questions.push(question);
        self.pending_questions.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.question_id.cmp(&b.question_id))
        });
        if self
            .pending_questions
            .first()
            .map(|pending| &pending.question_id)
            != previous_first.as_ref()
        {
            self.question_selection = 0;
            self.question_selected.clear();
        }
    }

    pub fn hydrate_questions(&mut self, questions: Vec<PendingQuestionInfo>) {
        self.hydrate_waiting(
            Waiting::Answer,
            questions.iter().map(|pending| {
                (
                    pending.question_id.clone(),
                    pending.thread_id,
                    pending.root_thread_id,
                )
            }),
        );
        self.pending_questions = questions
            .into_iter()
            .filter(|pending| {
                pending.root_thread_id == Some(self.root_thread_id)
                    || pending.thread_id == self.thread.id
            })
            .collect();
        self.pending_questions.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.question_id.cmp(&b.question_id))
        });
        self.question_selection = 0;
        self.question_selected.clear();
        if let Some(pending) = self.pending_questions.first() {
            pending.prompt.clone_into(&mut self.notice);
        }
    }

    /// Remove one resolved prompt and immediately expose the next queued
    /// question, if any.
    pub fn question_answered(&mut self, id: &str) {
        self.waiting.remove(id);
        let resolved_notification = self
            .pending_questions
            .iter()
            .find(|pending| pending.question_id == id)
            .map(|pending| format!("Haarness needs your answer: {}", pending.prompt));
        let removed_first = self
            .pending_questions
            .first()
            .is_some_and(|pending| pending.question_id == id);
        let previous_len = self.pending_questions.len();
        self.pending_questions
            .retain(|pending| pending.question_id != id);
        if let Some(message) = resolved_notification {
            self.dismiss_notification(&message);
        }
        if self.pending_questions.len() != previous_len && removed_first {
            self.question_selection = 0;
            self.question_selected.clear();
            self.notice = self
                .pending_questions
                .first()
                .map_or_else(|| "Answer sent".into(), |pending| pending.prompt.clone());
        }
    }

    pub fn question_resolved(&mut self, id: &str) {
        let removed_first = self
            .pending_questions
            .first()
            .is_some_and(|pending| pending.question_id == id);
        let previous_len = self.pending_questions.len();
        self.pending_questions
            .retain(|pending| pending.question_id != id);
        if self.pending_questions.len() != previous_len && removed_first {
            self.question_selection = 0;
            self.question_selected.clear();
            self.notice = self.pending_questions.first().map_or_else(
                || "Question resolved".into(),
                |pending| pending.prompt.clone(),
            );
        }
    }
    /// Total token usage across this thread's turns.
    pub fn usage_totals(&self) -> (u64, u64) {
        self.items.iter().fold((0, 0), |(input, output), item| {
            if let ItemData::Usage {
                input_tokens,
                output_tokens,
                ..
            } = &item.data
            {
                (input + input_tokens, output + output_tokens)
            } else {
                (input, output)
            }
        })
    }
    /// The newest timed request of this chat: how long it took to start
    /// answering and, when its output streamed for at least half a second,
    /// how fast that output came: the tokens that streamed per second. A
    /// shorter stream is mostly one burst, and dividing by it reads as an
    /// absurd rate.
    pub fn request_pace(&self) -> Option<(u64, Option<f64>)> {
        self.items.iter().rev().find_map(|item| match &item.data {
            ItemData::Usage {
                output_tokens,
                first_token_ms: Some(first_token_ms),
                stream_ms,
                stream_tokens,
                ..
            } if item.thread_id == self.thread.id => {
                let streamed = stream_tokens.unwrap_or(*output_tokens);
                #[allow(clippy::cast_precision_loss)] // display-only rate
                let speed = stream_ms
                    .filter(|stream_ms| *stream_ms >= 500 && streamed > 0)
                    .map(|stream_ms| streamed as f64 / (stream_ms as f64 / 1000.0));
                Some((*first_token_ms, speed))
            }
            _ => None,
        })
    }
    /// The newest timed request as one line ("first token 1.2s · 85 tok/s"),
    /// for the sidebar and `/usage`.
    pub fn request_pace_label(&self) -> Option<String> {
        let (first_token_ms, speed) = self.request_pace()?;
        let language = self.language;
        let mut parts = vec![language.format(
            "first token {time}",
            &[("time", &crate::view::span::seconds_label(first_token_ms))],
        )];
        if let Some(speed) = speed {
            parts.push(language.format(
                "{speed} tok/s",
                &[("speed", &crate::view::span::speed_label(speed))],
            ));
        }
        Some(parts.join(" · "))
    }
    /// Total cost of this thread when the models carried pricing.
    pub fn usage_cost(&self) -> Option<f64> {
        let mut total = 0.0;
        let mut priced = false;
        for item in &self.items {
            if let ItemData::Usage {
                cost_usd: Some(cost),
                ..
            } = &item.data
            {
                total += cost;
                priced = true;
            }
        }
        priced.then_some(total)
    }
    /// Context the next request carries against the model's window, 0..=1:
    /// the last provider report plus an estimate of what was added since,
    /// measured as the runtime measures it for compaction.
    pub fn context_fill(&self) -> Option<f64> {
        let revert = self.thread.revert.as_ref().map(|revert| revert.sequence);
        let key = (
            self.items.len(),
            self.items.last().map_or(0, |item| item.sequence),
            revert,
        );
        let tokens = match self.context_tokens.get() {
            Some((cached, tokens)) if cached == key => tokens,
            _ => {
                let retained = self
                    .items
                    .iter()
                    .filter(|item| revert.is_none_or(|sequence| item.sequence <= sequence))
                    .cloned()
                    .collect::<Vec<_>>();
                let tokenizer = haarness_protocol::tokens::Tokenizer::for_model(&self.thread.model);
                let tokens = haarness_protocol::tokens::context(&retained, tokenizer).tokens;
                self.context_tokens.set(Some((key, tokens)));
                tokens
            }
        };
        if tokens == 0 {
            return None;
        }
        let window = self
            .providers
            .iter()
            .find(|provider| provider.id == self.thread.provider)?
            .models
            .iter()
            .find(|model| {
                self.thread.model.starts_with(&model.id) || model.id == self.thread.model
            })?
            .context_window
            .filter(|window| *window > 0)?;
        #[allow(clippy::cast_precision_loss)] // display meter only
        Some((tokens as f64 / window as f64).clamp(0.0, 1.0))
    }
    pub fn agent(&self) -> &str {
        self.agents
            .get(self.agent_index)
            .map_or(self.thread.agent.as_str(), |agent| agent.id.as_str())
    }

    /// Stable identity for a real child task. The task prompt is its title;
    /// the child profile is secondary metadata. Tool names belong to live
    /// activity and must never replace this label in the sidebar or picker.
    pub fn child_task_identity(&self, call_id: &str, task: &ChildTask) -> (String, String) {
        let (prompt, requested_agent) = self
            .items
            .iter()
            .find_map(|item| match &item.data {
                ItemData::ToolCall {
                    call_id: candidate,
                    name,
                    arguments,
                    ..
                } if candidate == call_id && name == "task" => Some((
                    arguments.get("prompt").and_then(serde_json::Value::as_str),
                    arguments.get("agent").and_then(serde_json::Value::as_str),
                )),
                _ => None,
            })
            .unwrap_or((None, None));
        let child_thread = self
            .threads
            .iter()
            .find(|thread| thread.id == task.child_thread_id);
        let agent_id = requested_agent
            .or_else(|| child_thread.map(|thread| thread.agent.as_str()))
            .unwrap_or("agent");
        let agent_name = self
            .agents
            .iter()
            .find(|agent| agent.id == agent_id)
            .map_or(agent_id, |agent| agent.name.as_str())
            .to_owned();
        let title = prompt
            .and_then(|value| value.lines().map(str::trim).find(|line| !line.is_empty()))
            .or_else(|| {
                child_thread
                    .map(|thread| thread.title.trim())
                    .filter(|title| !title.is_empty() && *title != "Subagent task")
            })
            .unwrap_or(&agent_name)
            .to_owned();
        (title, agent_name)
    }

    pub fn cycle_agent(&mut self) {
        if let Some(index) = (1..=self.agents.len())
            .map(|offset| (self.agent_index + offset) % self.agents.len())
            .find(|&index| self.agents[index].selectable())
        {
            self.agent_index = index;
            self.notice = format!("Agent: {}", self.agent());
        }
    }

    pub fn cycle_agent_reverse(&mut self) {
        if let Some(index) = (1..=self.agents.len())
            .map(|offset| (self.agent_index + self.agents.len() - offset) % self.agents.len())
            .find(|&index| self.agents[index].selectable())
        {
            self.agent_index = index;
            self.notice = format!("Agent: {}", self.agent());
        }
    }

    pub fn open_overlay(&mut self, overlay: Overlay) {
        self.overlay = overlay;
        self.overlay_filter.clear();
        self.overlay_scroll = 0;
        self.max_overlay_scroll.set(0);
        self.selection = 0;
        // Open selectors on the current choice, not an arbitrary first row.
        self.selection = match self.overlay {
            Overlay::Threads => self
                .filtered_threads()
                .iter()
                .position(|thread| thread.id == self.thread.id)
                .unwrap_or(0),
            Overlay::Models => self
                .filtered_models()
                .iter()
                .position(|model| {
                    model == &format!("{}/{}", self.thread.provider, self.thread.model)
                })
                .unwrap_or(0),
            Overlay::Agents => self
                .filtered_agents()
                .iter()
                .position(|agent| agent.id == self.agent())
                .unwrap_or(0),
            Overlay::Themes => self
                .filtered_themes()
                .iter()
                .position(|(name, variant, _)| {
                    name == &self.theme_name && variant == &self.theme_variant
                })
                .unwrap_or(0),
            Overlay::Options {
                kind: OptionsKind::ReasoningEffort,
                ref entries,
                ..
            } => {
                let current = if self.thread.ultracode {
                    "ultracode"
                } else {
                    self.reasoning_effort.as_deref().unwrap_or("off")
                };
                self.filtered_options(entries)
                    .iter()
                    .position(|entry| entry.value == current)
                    .unwrap_or(0)
            }
            Overlay::Options {
                kind: OptionsKind::Language,
                ref entries,
                ..
            } => self
                .filtered_options(entries)
                .iter()
                .position(|entry| entry.value == self.language.code())
                .unwrap_or(0),
            Overlay::Options {
                kind: OptionsKind::Workspaces,
                ref entries,
                ..
            } => self
                .workspace_catalog
                .as_ref()
                .and_then(|catalog| catalog.current_workspace_id.as_deref())
                .and_then(|current| {
                    self.filtered_options(entries)
                        .iter()
                        .position(|entry| entry.value == format!("workspace:{current}"))
                })
                .unwrap_or(0),
            _ => 0,
        };
        if matches!(self.overlay, Overlay::Themes) {
            self.theme_preview_restore =
                Some((self.theme_name.clone(), self.theme_variant.clone()));
            self.preview_selected_theme();
        }
    }

    pub fn close_overlay(&mut self) {
        self.remember_message_browser();
        // Leaving the picker abandons the preview. A committed choice is
        // captured before the overlay closes and re-applied afterwards, so
        // restoring here only ever undoes a browse.
        if let Some((name, variant)) = self.theme_preview_restore.take() {
            self.apply_theme(&name, &variant);
        }
        self.overlay = Overlay::None;
        self.overlay_filter.clear();
        self.overlay_scroll = 0;
        self.max_overlay_scroll.set(0);
        self.selection = 0;
    }

    pub fn move_selection(&mut self, direction: isize) {
        if matches!(self.overlay, Overlay::None) {
            // The person chose a row: a returned pick's row no longer takes
            // the highlight when its listing comes.
            self.mention_resources.want = None;
        }
        let length = self.current_selection_length();
        if length == 0 {
            self.selection = 0;
            return;
        }

        let current = self.selection.min(length - 1);
        if matches!(self.overlay, Overlay::None) {
            // Composer autocomplete is intentionally cyclic: its short list
            // behaves like inline completion rather than a scrollable page.
            if direction < 0 {
                let distance = direction.unsigned_abs() % length;
                self.selection = (current + length - distance) % length;
            } else {
                self.selection = (current + direction.cast_unsigned() % length) % length;
            }
        } else if direction < 0 {
            // Explicit selectors are paged dialogs. Wrapping from the last
            // session back to the first makes wheel/PageDown input look like
            // the viewport jumped or ignored its boundary.
            self.selection = current.saturating_sub(direction.unsigned_abs());
        } else {
            self.selection = current
                .saturating_add(direction.cast_unsigned())
                .min(length - 1);
        }
        self.preview_selected_theme();
    }

    pub(crate) fn current_selection_length(&self) -> usize {
        match self.overlay {
            Overlay::Commands => self.filtered_slash_entries(&self.overlay_filter).len(),
            Overlay::Threads => self.filtered_threads().len(),
            Overlay::Models => self.filtered_models().len(),
            Overlay::Agents => self.filtered_agents().len(),
            Overlay::Themes => self.filtered_themes().len(),
            Overlay::Workflow { .. }
            | Overlay::ProviderLogin
            | Overlay::RemoteControl
            | Overlay::Chrome
            | Overlay::SideChat => 0,
            Overlay::Options { ref entries, .. } => self.filtered_options(entries).len(),
            Overlay::None | Overlay::Panel { .. } | Overlay::Diff(_) => self
                .slash_suggestions()
                .map_or_else(|| self.shown_mentions().len(), |commands| commands.len()),
        }
    }

    /// The `@` list rows the composer shows: none while Esc hides the list,
    /// so a listing that comes meanwhile is neither picked nor browsed.
    pub(crate) fn shown_mentions(&self) -> &[String] {
        if self.autocomplete_hidden {
            &[]
        } else {
            &self.mention_suggestions
        }
    }

    pub fn slash_suggestions(&self) -> Option<Vec<SlashEntry>> {
        if self.autocomplete_hidden {
            return None;
        }
        let text = self.composer.text();
        let query = text.strip_prefix('/')?;
        if query.contains(char::is_whitespace) {
            return None;
        }
        Some(self.filtered_slash_entries(query))
    }

    /// Add canonical attachment paths without duplicating an existing chip.
    pub fn add_attachments(&mut self, paths: Vec<String>) -> usize {
        let before = self.attachments.len();
        for path in paths {
            if !self.attachments.contains(&path) {
                self.attachments.push(path);
            }
        }
        self.attachments.len() - before
    }

    /// Built-in commands plus the server's custom commands, filtered.
    pub fn filtered_slash_entries(&self, query: &str) -> Vec<SlashEntry> {
        let query = query.trim().to_ascii_lowercase();
        let mut entries: Vec<SlashEntry> = COMMANDS
            .iter()
            .filter(|entry| {
                query.is_empty()
                    || entry.name.starts_with(&query)
                    || entry.aliases.iter().any(|alias| alias.starts_with(&query))
                    || entry.description.to_ascii_lowercase().contains(&query)
                    || self
                        .language
                        .text(entry.description)
                        .to_lowercase()
                        .contains(&query)
            })
            .map(|entry| SlashEntry {
                name: entry.name.to_owned(),
                // Read at render time, keyed by the English text, so a
                // language switch reaches an open menu.
                description: self.language.text(entry.description).to_owned(),
            })
            .collect();
        for command in &self.server_commands {
            let matches = query.is_empty()
                || command.name.to_ascii_lowercase().starts_with(&query)
                || command.description.to_ascii_lowercase().contains(&query);
            if matches && !entries.iter().any(|entry| entry.name == command.name) {
                let hint = command
                    .argument_hint
                    .as_deref()
                    .map_or_else(String::new, |hint| format!(" {hint}"));
                entries.push(SlashEntry {
                    name: command.name.clone(),
                    description: format!(
                        "{}{}{hint}",
                        command
                            .plugin
                            .as_deref()
                            .map_or_else(String::new, |plugin| format!("{plugin} · ")),
                        command.description
                    ),
                });
            }
        }
        for workflow in &self.workflow_definitions {
            let invocation = workflow.invocation.to_ascii_lowercase();
            let matches = query.is_empty()
                || invocation.starts_with(&query)
                || workflow
                    .meta
                    .description
                    .as_deref()
                    .is_some_and(|description| description.to_ascii_lowercase().contains(&query))
                || workflow
                    .meta
                    .when_to_use
                    .as_deref()
                    .is_some_and(|when| when.to_ascii_lowercase().contains(&query));
            if matches
                && !entries
                    .iter()
                    .any(|entry| entry.name.eq_ignore_ascii_case(&workflow.invocation))
            {
                let description = workflow
                    .meta
                    .description
                    .as_deref()
                    .unwrap_or_else(|| self.language.text("Run reusable workflow"));
                let arguments = if workflow.meta.arguments.is_empty() {
                    String::new()
                } else {
                    format!(
                        " · args {}",
                        workflow
                            .meta
                            .arguments
                            .keys()
                            .map(|key| format!("<{key}>"))
                            .collect::<Vec<_>>()
                            .join(" ")
                    )
                };
                entries.push(SlashEntry {
                    name: workflow.invocation.clone(),
                    description: format!(
                        "{} · {description}{arguments}",
                        self.language.text("Workflow")
                    ),
                });
            }
        }
        // Search descriptions for discovery, but never let a loose prose
        // match visually outrank the exact command that Enter will execute.
        // Stable sorting preserves catalog order inside each relevance tier.
        if !query.is_empty() {
            entries.sort_by_key(|entry| {
                let spec = COMMANDS.iter().find(|spec| spec.name == entry.name);
                if entry.name.eq_ignore_ascii_case(&query) {
                    0
                } else if spec.is_some_and(|spec| {
                    spec.aliases
                        .iter()
                        .any(|alias| alias.eq_ignore_ascii_case(&query))
                }) {
                    1
                } else if entry.name.to_ascii_lowercase().starts_with(&query) {
                    2
                } else if spec
                    .is_some_and(|spec| spec.aliases.iter().any(|alias| alias.starts_with(&query)))
                {
                    3
                } else {
                    4
                }
            });
        }
        entries
    }

    pub fn filtered_threads(&self) -> Vec<&Thread> {
        let query = self.overlay_filter.to_ascii_lowercase();
        let mut threads: Vec<_> = self
            .threads
            .iter()
            // Child-agent threads have their own drill-down (/tasks); the
            // switcher lists conversations.
            .filter(|thread| !thread.is_subagent)
            .filter(|thread| {
                self.session_include_archived || !thread.archived || thread.id == self.thread.id
            })
            .filter(|thread| {
                query.is_empty()
                    || thread.title.to_ascii_lowercase().contains(&query)
                    || thread.id.to_string().contains(&query)
            })
            .collect();
        // Chats that wait for a person lead, as Upstream lists them; each
        // part stays newest first.
        let waiting = self.waiting_chats();
        threads.sort_by(|left, right| {
            waiting
                .contains_key(&right.id)
                .cmp(&waiting.contains_key(&left.id))
                .then_with(|| right.updated_at.cmp(&left.updated_at))
        });
        threads
    }

    /// The listed chat a waiting request is answered in: the nearest thread
    /// up its chain that is not a child agent, as Desktop's
    /// `conversationRoot` finds it (a fork is a chat of its own). A thread
    /// this client has not listed falls back to the server's root.
    fn waiting_chat(
        threads: &std::collections::HashMap<ThreadId, &Thread>,
        request: &WaitingRequest,
    ) -> ThreadId {
        let mut id = request.thread_id;
        for _ in 0..16 {
            match threads.get(&id) {
                Some(thread) if thread.is_subagent => match thread.parent_id {
                    Some(parent) => id = parent,
                    None => break,
                },
                Some(_) => break,
                None => return request.root_thread_id.unwrap_or(id),
            }
        }
        id
    }

    /// What each chat waits on from a person: an approval over a question.
    /// Built once per list or frame, not per row.
    pub fn waiting_chats(&self) -> std::collections::HashMap<ThreadId, Waiting> {
        let mut chats = std::collections::HashMap::new();
        if self.waiting.is_empty() {
            return chats;
        }
        let threads = self
            .threads
            .iter()
            .map(|thread| (thread.id, thread))
            .collect();
        for request in self.waiting.values() {
            let kind = chats
                .entry(Self::waiting_chat(&threads, request))
                .or_insert(request.kind);
            *kind = (*kind).max(request.kind);
        }
        chats
    }

    /// Replace what waits of one kind with the server's complete list.
    pub(crate) fn hydrate_waiting(
        &mut self,
        kind: Waiting,
        requests: impl IntoIterator<Item = (String, ThreadId, Option<ThreadId>)>,
    ) {
        let anchor = self.session_list_anchor();
        self.waiting.retain(|_, request| request.kind != kind);
        for (id, thread_id, root_thread_id) in requests {
            self.waiting.insert(
                id,
                WaitingRequest {
                    thread_id,
                    root_thread_id,
                    kind,
                },
            );
        }
        self.keep_session_list_anchor(anchor);
    }

    /// The chat under the session list's cursor, so a change that reorders
    /// the list (a chat starts or stops waiting, or is updated) can keep the
    /// cursor on it.
    pub(crate) fn session_list_anchor(&self) -> Option<ThreadId> {
        if !matches!(self.overlay, Overlay::Threads) {
            return None;
        }
        self.filtered_threads()
            .get(self.selection)
            .map(|thread| thread.id)
    }

    pub(crate) fn keep_session_list_anchor(&mut self, anchor: Option<ThreadId>) {
        if let Some(id) = anchor
            && matches!(self.overlay, Overlay::Threads)
            && let Some(index) = self
                .filtered_threads()
                .iter()
                .position(|thread| thread.id == id)
        {
            self.selection = index;
        }
    }

    /// Compact, truthful metadata for a session-list row. Haarness only calls
    /// a session "working" when local REST/SSE state proves it; older sessions
    /// are never guessed active from their update timestamp. `waiting` is
    /// what the chat waits on, from one [`Self::waiting_chats`] for the list.
    pub fn session_detail(&self, thread: &Thread, waiting: Option<Waiting>) -> String {
        let current = thread.id == self.thread.id
            || (!self.parent_stack.is_empty() && thread.id == self.root_thread_id);
        let working =
            (current && self.active_turn.is_some()) || self.active_tree_turn_count(thread.id) > 0;
        let language = self.language;
        let mut detail = Vec::new();
        // What the chat waits for comes first, where truncation never
        // reaches it, in the words the activity line uses. Its turn is
        // stopped on the reader, so it says that rather than "working", as
        // Desktop's waiting rows do.
        match waiting {
            Some(Waiting::Approval) => {
                detail.push(language.text("waiting for approval").to_owned());
            }
            Some(Waiting::Answer) => {
                detail.push(language.text("waiting for your answer").to_owned());
            }
            None => {}
        }
        if current {
            detail.push(language.text("current").to_owned());
        }
        if working && waiting.is_none() {
            detail.push(language.text("working").to_owned());
        }
        if thread.archived {
            detail.push(language.text("archived").to_owned());
        }
        detail.push(thread.agent.clone());
        detail.push(self.model_label(&format!("{}/{}", thread.provider, thread.model)));
        let age = (chrono::Utc::now() - thread.updated_at)
            .num_seconds()
            .max(0);
        let ago = |key: &str, count: i64| language.format(key, &[("count", &count.to_string())]);
        detail.push(if age < 60 {
            language.text("updated now").to_owned()
        } else if age < 3_600 {
            ago("updated {count}m ago", age / 60)
        } else if age < 86_400 {
            ago("updated {count}h ago", age / 3_600)
        } else {
            ago("updated {count}d ago", age / 86_400)
        });
        let id = thread.id.to_string();
        detail.push(crate::view::span::short_id(&id).to_owned());
        detail.join(" · ")
    }

    /// Open safe actions for the highlighted session. Permanent deletion is
    /// intentionally absent: the current API offers reversible archival.
    pub fn open_selected_session_actions(&mut self) {
        let Some(thread) = self
            .filtered_threads()
            .get(self.selection)
            .copied()
            .cloned()
        else {
            self.notice = "No session selected".into();
            return;
        };
        let id = thread.id.to_string();
        let language = self.language;
        let text = |key: &str| language.text(key).to_owned();
        let mut entries = Vec::new();
        if thread.id != self.thread.id {
            entries.push(OptionEntry {
                label: if thread.archived {
                    text("Restore and resume")
                } else {
                    text("Resume")
                },
                description: text("Open this session and restore its draft"),
                value: format!("resume:{id}"),
            });
        }
        entries.extend([
            OptionEntry {
                label: text("Fork and resume"),
                description: text("Create a new session from this history"),
                value: format!("fork:{id}"),
            },
            OptionEntry {
                label: text("Rename"),
                description: text("Prefill a rename command for this session"),
                value: format!("rename:{id}"),
            },
            OptionEntry {
                label: if thread.archived {
                    text("Unarchive")
                } else {
                    text("Archive")
                },
                description: if thread.id == self.thread.id {
                    text("Hide this session and open a fresh one")
                } else if thread.archived {
                    text("Return this session to the normal switcher")
                } else {
                    text("Hide this session without deleting it")
                },
                value: format!(
                    "{}:{id}",
                    if thread.archived {
                        "unarchive"
                    } else {
                        "archive"
                    }
                ),
            },
            OptionEntry {
                label: text("Copy session ID"),
                description: id.clone(),
                value: format!("copy:{id}"),
            },
        ]);
        self.open_overlay(Overlay::Options {
            title: thread.title,
            kind: OptionsKind::SessionActions,
            entries,
        });
    }

    pub fn model_label(&self, qualified: &str) -> String {
        let (provider, id) = qualified.split_once('/').unwrap_or(("", qualified));
        let supplied = self
            .providers
            .iter()
            .find(|p| p.id == provider)
            .and_then(|p| p.models.iter().find(|m| m.id == id))
            .map(|m| m.display_name.as_str());
        haarness_protocol::model_display_name(id, supplied)
    }

    pub fn reasoning_efforts(&self) -> &[String] {
        self.providers
            .iter()
            .find(|provider| provider.id == self.thread.provider)
            .and_then(|provider| {
                provider
                    .models
                    .iter()
                    .find(|model| model.id == self.thread.model)
            })
            .filter(|model| model.reasoning)
            .map_or(&[], |model| model.reasoning_efforts.as_slice())
    }

    pub fn normalize_reasoning(&mut self) {
        if self
            .reasoning_effort
            .as_ref()
            .is_some_and(|effort| !self.reasoning_efforts().contains(effort))
        {
            self.reasoning_effort = None;
        }
    }

    pub fn filtered_models(&self) -> Vec<String> {
        let query = self.overlay_filter.to_ascii_lowercase();
        let mut providers: Vec<_> = self.providers.iter().filter(|p| p.authenticated).collect();
        providers.sort_by_key(|provider| !provider.authenticated);
        let mut models: Vec<String> = providers
            .iter()
            .flat_map(|provider| {
                provider
                    .models
                    .iter()
                    .map(move |model| format!("{}/{}", provider.id, model.id))
            })
            .filter(|model| {
                query.is_empty()
                    || model.to_ascii_lowercase().contains(&query)
                    || haarness_protocol::model_display_id(model)
                        .to_ascii_lowercase()
                        .contains(&query)
                    || self
                        .model_label(model)
                        .to_ascii_lowercase()
                        .contains(&query)
            })
            .collect();
        models.sort_by_key(|model| {
            (
                !self.model_favorites.contains(model),
                self.model_recents
                    .iter()
                    .position(|m| m == model)
                    .unwrap_or(usize::MAX),
            )
        });
        models
    }

    pub fn filtered_agents(&self) -> Vec<&AgentProfile> {
        let query = self.overlay_filter.to_ascii_lowercase();
        self.agents
            .iter()
            .filter(|agent| {
                agent.selectable()
                    && (query.is_empty()
                        || agent.id.to_ascii_lowercase().contains(&query)
                        || agent.name.to_ascii_lowercase().contains(&query))
            })
            .collect()
    }

    pub fn selected_overlay_choice(&self) -> Option<OverlayChoice> {
        match self.overlay {
            Overlay::Commands => selected(
                &self.filtered_slash_entries(&self.overlay_filter),
                self.selection,
            )
            .map(|entry| OverlayChoice::Command(format!("/{}", entry.name))),
            Overlay::Threads => selected(&self.filtered_threads(), self.selection)
                .map(|thread| OverlayChoice::Thread(Box::new((*thread).clone()))),
            Overlay::Models => selected(&self.filtered_models(), self.selection)
                .cloned()
                .map(OverlayChoice::Model),
            Overlay::Agents => selected(&self.filtered_agents(), self.selection)
                .map(|agent| OverlayChoice::Agent(agent.id.clone())),
            Overlay::Themes => selected(&self.filtered_themes(), self.selection)
                .map(|(name, variant, _)| OverlayChoice::Theme(name.clone(), variant.clone())),
            Overlay::Options {
                kind, ref entries, ..
            } if matches!(
                kind,
                OptionsKind::ManagementField
                    | OptionsKind::ConnectApiKey
                    | OptionsKind::WorkspaceCreateName
                    | OptionsKind::WorkspaceCreateCustomBase
            ) =>
            {
                Some(OverlayChoice::Option(kind, self.overlay_filter.clone()))
            }
            Overlay::Options {
                kind, ref entries, ..
            } => selected(&self.filtered_options(entries), self.selection)
                .map(|entry| OverlayChoice::Option(kind, entry.value.clone())),
            Overlay::None
            | Overlay::Panel { .. }
            | Overlay::Diff(_)
            | Overlay::Workflow { .. }
            | Overlay::ProviderLogin
            | Overlay::RemoteControl
            | Overlay::Chrome
            | Overlay::SideChat => None,
        }
    }
}

/// Root of a thread's parent chain, resolved against the known thread list.
/// Unknown parents resolve to the deepest reachable ancestor.
/// "OpenAI is not connected · /connect openai": the provider's own name, not
/// its id, so Cloud reads as "Haarness Cloud" rather than an internal
/// "haarness", and the command that connects exactly that provider.
pub(crate) fn not_connected(providers: &[ProviderInfo], id: &str) -> String {
    let name = providers
        .iter()
        .find(|provider| provider.id == id)
        .map_or_else(
            || haarness_protocol::provider_display_id(id).to_owned(),
            |provider| provider.name.clone(),
        );
    format!("{name} is not connected · /connect {id}")
}

/// The status of a chat whose provider cannot answer. A chat starts on the
/// server's default provider, which nobody chose on a fresh install, so it is
/// named only once something else is connected.
pub(crate) fn unconnected_notice(providers: &[ProviderInfo], id: &str) -> String {
    if providers.is_empty()
        || providers
            .iter()
            .any(|provider| provider.authenticated && !provider.models.is_empty())
    {
        not_connected(providers, id)
    } else {
        "Connect a provider to start · /connect".into()
    }
}

pub fn compute_root(threads: &[Thread], mut id: ThreadId) -> ThreadId {
    for _ in 0..16 {
        match threads
            .iter()
            .find(|thread| thread.id == id)
            .and_then(|thread| thread.parent_id)
        {
            Some(parent) => id = parent,
            None => break,
        }
    }
    id
}

pub(crate) fn workflow_status(status: WorkflowRunStatus) -> &'static str {
    match status {
        WorkflowRunStatus::Running => "running",
        WorkflowRunStatus::Completed => "completed",
        WorkflowRunStatus::Failed => "failed",
        WorkflowRunStatus::Cancelled => "cancelled",
        WorkflowRunStatus::Paused => "paused",
        WorkflowRunStatus::Interrupted => "interrupted",
    }
}

fn workflow_origin_priority(
    run: &WorkflowRun,
    current_thread: ThreadId,
    current_root: ThreadId,
) -> u8 {
    match run.definition.launch_thread_id.or(run.thread_id) {
        Some(id) if id == current_thread => 0,
        Some(id) if id == current_root => 1,
        _ => 2,
    }
}

pub(crate) fn workflow_status_marker(status: WorkflowRunStatus) -> &'static str {
    match status {
        WorkflowRunStatus::Running => "●",
        WorkflowRunStatus::Completed => "✓",
        WorkflowRunStatus::Failed => "✗",
        WorkflowRunStatus::Cancelled => "⊘",
        WorkflowRunStatus::Paused => "Ⅱ",
        WorkflowRunStatus::Interrupted => "○",
    }
}

fn relative_time(time: chrono::DateTime<chrono::Utc>) -> String {
    let age = (chrono::Utc::now() - time).num_seconds().max(0);
    if age < 60 {
        "updated now".into()
    } else if age < 3_600 {
        format!("updated {}m ago", age / 60)
    } else if age < 86_400 {
        format!("updated {}h ago", age / 3_600)
    } else {
        format!("updated {}d ago", age / 86_400)
    }
}

impl App {
    /// The permission prompt y/a/w/n answer next: oldest pending.
    pub fn set_auto_permissions(&mut self, enabled: bool) {
        self.keep_permission_choice(|app| app.auto_permissions = enabled);
        self.notice = if enabled {
            "Auto on for this TUI launch · new requests allowed once · pending requests stay manual · ctrl+y to review"
        } else {
            "Review mode · permission requests need approval · ctrl+y to switch"
        }.into();
    }

    /// Change what the card offers (the chat's mode) and keep the answer it
    /// had in focus: the same choice where it is still offered, otherwise
    /// Allow once, never whichever neighbour slid into its place. Once + Auto
    /// staged in a chat that is now in Auto has nothing left to confirm.
    pub(crate) fn keep_permission_choice(&mut self, change: impl FnOnce(&mut Self)) {
        // While an answer is staged the selection is confirm or cancel.
        let before = PermissionChoice::available(self);
        let focused = before
            .get(
                self.permission_selection
                    .min(before.len().saturating_sub(1)),
            )
            .copied()
            .filter(|_| self.confirm_always.is_none());
        change(self);
        if self.first_pending_permission().is_none() {
            return;
        }
        if let Some(staged) = self.confirm_always {
            if !(self.auto_permissions && staged == PermissionChoice::Auto.key()) {
                return;
            }
            self.confirm_always = None;
            self.update_permission_notice();
        }
        let after = PermissionChoice::available(self);
        self.permission_selection = focused
            .and_then(|choice| after.iter().position(|item| *item == choice))
            .or_else(|| {
                after
                    .iter()
                    .position(|item| *item == PermissionChoice::Once)
            })
            .unwrap_or(0);
    }

    pub fn first_pending_permission(&self) -> Option<&PendingPermissionInfo> {
        self.pending_permissions.first()
    }

    /// The approval card's subject scroll for request `id`: its offset and
    /// whether it is expanded. A request nobody moved starts at the top.
    pub fn permission_scroll_of(&self, id: &str) -> (usize, bool) {
        self.permission_scroll
            .as_ref()
            .filter(|scroll| scroll.permission_id == id)
            .map_or((0, false), |scroll| (scroll.offset, scroll.expanded))
    }

    /// Scroll the first request's subject by `rows` (negative is up), within
    /// what the last frame could show. False when there is nothing to move.
    pub fn scroll_permission_subject(&mut self, rows: isize) -> bool {
        let fit = self.permission_fit.get();
        let Some(id) = self
            .first_pending_permission()
            .map(|pending| pending.permission_id.clone())
        else {
            return false;
        };
        if fit.max_offset == 0 {
            return false;
        }
        let (offset, expanded) = self.permission_scroll_of(&id);
        let offset = offset
            .min(fit.max_offset)
            .saturating_add_signed(rows)
            .min(fit.max_offset);
        self.permission_scroll = Some(PermissionScroll {
            permission_id: id,
            offset,
            expanded,
        });
        true
    }

    /// Expand the first request's subject to every row the card can spare,
    /// or collapse it back. Expanding is offered only where it shows more.
    pub fn toggle_permission_subject(&mut self) -> bool {
        let Some(id) = self
            .first_pending_permission()
            .map(|pending| pending.permission_id.clone())
        else {
            return false;
        };
        let (offset, expanded) = self.permission_scroll_of(&id);
        if !expanded && !self.permission_fit.get().expandable {
            return false;
        }
        self.permission_scroll = Some(PermissionScroll {
            permission_id: id,
            offset,
            expanded: !expanded,
        });
        true
    }
    /// Queued follow-ups the running turn has not consumed yet, oldest first.
    /// These are delivery state, not conversation content: they wait in the
    /// strip above the composer and only join the transcript (at their
    /// durable position) once a `QueueConsumed` marker lands.
    pub fn pending_follow_ups(&self) -> Vec<&Item> {
        if self.active_turn.is_none() {
            return Vec::new();
        }
        self.items
            .iter()
            .filter(|item| {
                matches!(
                    item.data,
                    ItemData::UserMessage {
                        queued: true,
                        synthetic: false,
                        ..
                    }
                ) && !self.items.iter().any(|candidate| {
                    matches!(
                        candidate.data,
                        ItemData::QueueConsumed { item_id } if item_id == item.id
                    )
                })
            })
            .collect()
    }
    /// Child tasks still running (or waiting on an approval).
    pub fn running_children(&self) -> usize {
        self.children
            .values()
            .filter(|task| task.is_active())
            .count()
    }
}

impl App {
    /// The drag selection normalized to ((first_row, first_col), (last_row,
    /// last_col)) in char cells.
    #[must_use]
    pub fn normalized_selection(&self) -> Option<((usize, usize), (usize, usize))> {
        let ((ax, ay), (bx, by)) = self.selection_range?;
        let a = (ay as usize, ax as usize);
        let b = (by as usize, bx as usize);
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    /// Text under the current drag selection, extracted from the last frame.
    #[must_use]
    pub fn selected_text(&self) -> String {
        let Some(((row1, col1), (row2, col2))) = self.normalized_selection() else {
            return String::new();
        };
        let frame = self.last_frame.borrow();
        let mut out = Vec::new();
        for row in row1..=row2.min(frame.len().saturating_sub(1)) {
            let from = if row == row1 { col1 } else { 0 };
            let to = if row == row2 { col2 + 1 } else { usize::MAX };
            let mut cell = 0;
            let mut slice = String::new();
            let mut selected_base = false;
            for character in frame[row].chars() {
                let cells = character.width().unwrap_or(0);
                let next = cell + cells;
                // Mouse columns are display cells, not Unicode scalar
                // indexes. Intersect the selected cells with each glyph so a
                // CJK character or emoji is copied whole from either cell.
                let intersects = if cells == 0 {
                    selected_base
                } else {
                    next > from && cell < to
                };
                if intersects {
                    slice.push(character);
                }
                selected_base = intersects;
                cell = next;
                if cell >= to && cells > 0 {
                    // Keep immediately following combining marks, but no
                    // later visible glyphs.
                    selected_base = true;
                }
            }
            out.push(slice.trim_end().to_owned());
        }
        out.join("\n")
    }
}

pub fn default_connected_model(providers: &[ProviderInfo]) -> Option<(String, String)> {
    providers
        .iter()
        .filter(|provider| provider.authenticated)
        .min_by_key(|provider| match provider.id.as_str() {
            "openai" => 0,
            "anthropic" => 1,
            "gemini" => 2,
            _ => 3,
        })
        .and_then(|provider| {
            provider
                .models
                .first()
                .map(|model| (provider.id.clone(), model.id.clone()))
        })
}

/// The `batch` call a numbered child call id (`call_x:3`) belongs to.
pub(crate) fn batch_parent_call(call_id: &str) -> Option<&str> {
    call_id
        .rsplit_once(':')
        .filter(|(_, index)| !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit()))
        .map(|(parent, _)| parent)
}

pub fn selected<T>(values: &[T], index: usize) -> Option<&T> {
    values.get(index).or_else(|| values.first())
}
