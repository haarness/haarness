//! Stable wire contracts shared by every Haarness client.

mod approval;
mod archive;
mod btw;
mod cloud;
mod goal;
mod home;
mod input;
mod management;
mod model_names;
mod pty;
pub mod tokens;
mod tools;
mod workflow;
mod workspace;
pub use approval::*;
pub use archive::*;
pub use btw::*;
pub use cloud::*;
pub use goal::*;
pub use home::{
    BASE_CONFIG_DIR_ENV, base_config_dir, base_config_dir_from, base_config_dir_problem, home_dir,
};
pub use input::*;
pub use management::*;
pub use model_names::{
    model_display_id, model_display_name, provider_display_id, provider_storage_id,
};
pub use pty::*;
pub use tools::*;
pub use workflow::*;
pub use workspace::*;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use utoipa::{OpenApi, ToSchema};
use uuid::Uuid;

pub const SCHEMA_VERSION: &str = "1";
/// Server capability required by clients that expose Git review and
/// workspace snapshot undo/redo. Keeping this explicit lets a newly-built
/// client reject (and locally replace) a still-running daemon from before the
/// endpoints existed, even when the package and event schema versions match.
pub const WORKSPACE_HISTORY_CAPABILITY: &str = "workspace-history.v1";
/// Server capability for the path-safe stale workspace cleanup endpoint.
pub const WORKSPACE_CLEANUP_CAPABILITY: &str = "workspace-cleanup.v1";
/// Server capability for workspace-scoped interactive PTY sessions.
pub const PTY_CAPABILITY: &str = "pty.v1";
/// Server capability for [`CreateThreadRequest::id`]. Older servers ignore an
/// unknown `id` and choose their own, so a client that relies on its chosen id
/// checks this first or compares the returned one.
pub const THREAD_CREATE_ID_CAPABILITY: &str = "thread-create-id.v1";
/// Reasoning-effort names Haarness can send to a provider, lightest first.
/// Model catalogs advertise a per-model subset of this scale; a tier outside
/// it is a provider-app setting the public request APIs reject, so it never
/// reaches a request.
pub const REASONING_EFFORTS: [&str; 7] =
    ["none", "minimal", "low", "medium", "high", "xhigh", "max"];

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

id_type!(ThreadId);
id_type!(TurnId);
id_type!(ItemId);
id_type!(RequestId);

/// The title of a chat nobody has named yet. Haarness replaces it with a
/// generated title after the first prompt; clients may show their own words
/// for an untitled chat instead.
pub const UNTITLED_CHAT_TITLE: &str = "New chat";

/// The placeholder servers before 2026-10 stored for an untitled chat. Chats
/// created then still carry it in their database rows and archives.
pub const LEGACY_UNTITLED_CHAT_TITLE: &str = "New thread";

/// Whether `title` is the placeholder of a chat nobody has named, from this
/// server or an older one.
#[must_use]
pub fn is_untitled_chat_title(title: &str) -> bool {
    title == UNTITLED_CHAT_TITLE || title == LEGACY_UNTITLED_CHAT_TITLE
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    /// Schedule that created this run; independent of its editable title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<ScheduleOrigin>,
    pub id: ThreadId,
    pub title: String,
    pub parent_id: Option<ThreadId>,
    /// Delegated agent conversation; forks retain parent lineage but are not subagents.
    #[serde(default)]
    pub is_subagent: bool,
    /// Durable boundary between copied history and a fork's own continuation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_origin: Option<ForkOrigin>,
    pub archived: bool,
    pub agent: String,
    pub provider: String,
    pub model: String,
    /// Standing workflow-orchestration preference for subsequent turns.
    #[serde(default)]
    pub ultracode: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Present while the thread sits in a reverted state (undo pending
    /// confirmation by the next turn).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revert: Option<RevertState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleOrigin {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ForkOrigin {
    /// Source title at creation, retained when the source is unavailable.
    pub title: String,
    /// Last copied sequence, or zero for a fork with no copied history.
    pub sequence: u64,
    /// Original source item, not its newly assigned ID in the fork.
    pub item_id: Option<ItemId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RevertState {
    /// Items after this sequence soft-delete when the next turn starts.
    pub sequence: u64,
    /// Pre-revert backup snapshot consumed by unrevert.
    pub snapshot_id: String,
    /// Workspace state produced by undo. Redo uses it as the expected side of
    /// a conflict-checked delta before restoring `snapshot_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_snapshot_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: TurnId,
    pub thread_id: ThreadId,
    pub request_id: RequestId,
    pub status: TurnStatus,
    pub agent: String,
    pub provider: String,
    pub model: String,
    /// This turn runs with workflow-orchestration guidance. The flag is
    /// durable and provider-independent; runtimes resolve it to the strongest
    /// supported reasoning effort (or the provider default).
    #[serde(default)]
    pub ultracode: bool,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Estimates of the final prepared request, after request hooks, with the
/// weights of the model's tokenizer family. Provider framing is not included;
/// the reported input of the same request calibrates later estimates.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PromptUsage {
    pub system_tokens: u64,
    pub tool_tokens: u64,
    pub history_tokens: u64,
    pub media_attachments: u64,
    /// Estimated tokens of those attachments; zero in records made before
    /// media was estimated.
    #[serde(default)]
    pub media_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ItemData {
    UserMessage {
        text: String,
        /// Local media paths, file URLs, or data URLs attached to this
        /// message. `images` remains accepted when reading older databases
        /// and API clients.
        #[serde(default, alias = "images")]
        attachments: Vec<String>,
        /// True when the message arrived while a turn was already running and
        /// was queued for steering into that turn's next provider round.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        queued: bool,
        /// Internal prompt injected by the runtime rather than authored by the
        /// user. Clients hide it from transcript/history surfaces while the
        /// provider still receives its text as normal context.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        synthetic: bool,
        /// The line typed to run a slash command (`/review src/foo.rs`).
        /// `text` becomes the prompt the command expands to, which is what
        /// the model receives; clients show this line instead. Absent for
        /// ordinary messages and for messages saved before it was kept.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command_line: Option<String>,
    },
    /// Durable boundary recording that a queued user message was included in
    /// a provider-round snapshot. The original message keeps `queued=true` as
    /// provenance; this marker is its transient-queue acknowledgement.
    QueueConsumed {
        item_id: ItemId,
    },
    AssistantMessage {
        text: String,
    },
    Reasoning {
        text: String,
        /// How long the model thought before this text was complete: from
        /// the request (or the text or tool call before it) to the end of the
        /// reasoning, read off a monotonic clock. Absent in records made
        /// before it was measured.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
    ToolCall {
        call_id: String,
        name: String,
        arguments: Value,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        provider_metadata: Value,
    },
    ToolResult {
        call_id: String,
        name: String,
        output: String,
        is_error: bool,
        /// Host-produced structured result details. Native file tools use
        /// this for the actual before/after content rendered by clients;
        /// older persisted results decode as null.
        #[serde(default, skip_serializing_if = "Value::is_null")]
        metadata: Value,
    },
    /// Durable navigation edge between a parent `task` call and the child
    /// thread it created. Live `task.updated` events carry progress, while
    /// this item keeps completed children reopenable after the turn or a
    /// client restart.
    TaskLink {
        call_id: String,
        child_thread_id: ThreadId,
    },
    /// Durable lifecycle checkpoint for a task that may outlive the parent
    /// tool call. This keeps background state reconstructable after an SSE
    /// gap or client restart without treating the launch acknowledgement as
    /// the child's final result.
    TaskState {
        call_id: String,
        child_thread_id: ThreadId,
        child_turn_id: TurnId,
        /// running | background | done | failed | cancelled | interrupted
        state: String,
    },
    McpAction {
        server: String,
        action: String,
        detail: Value,
    },
    PermissionRequest {
        permission_id: String,
        operation: String,
        detail: Value,
        /// Permission category (bash, edit, external_directory, …).
        #[serde(default)]
        permission: String,
        /// Exact things being asked about (command texts, paths, globs).
        #[serde(default)]
        patterns: Vec<String>,
        /// Generalized rules an "always" answer would persist,
        /// e.g. `bash:git status *`.
        #[serde(default)]
        always_patterns: Vec<String>,
    },
    Question {
        question_id: String,
        prompt: String,
        choices: Vec<String>,
        /// Select any number of choices before submitting one reply.
        #[serde(default)]
        multiple: bool,
        /// Permit free-form text alongside the listed choices.
        #[serde(default)]
        custom: bool,
    },
    Compaction {
        summary: String,
        files_touched: Vec<String>,
        outstanding: Vec<String>,
        /// Exact task-list state at this compaction boundary. Older records
        /// decode without a checkpoint; a present checkpoint with an empty
        /// list intentionally clears all prior todos.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        todo_checkpoint: Option<TodoCheckpoint>,
        /// First raw transcript item retained verbatim after the summary.
        /// Sequence numbers remain stable inside a thread and across forks.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tail_start_sequence: Option<u64>,
    },
    Plan {
        markdown: String,
    },
    Review {
        findings: Vec<ReviewFinding>,
    },
    Error {
        message: String,
        retryable: bool,
    },
    /// Attachments a provider refused, left out of every later request of
    /// this thread so one bad image does not fail each turn. The attachments
    /// themselves stay in their messages; the model gets a short note instead.
    MediaOmitted {
        /// Hex SHA-256 of each omitted attachment source exactly as stored
        /// (a path or a data URL).
        fingerprints: Vec<String>,
        /// Display names of those attachments, for the notice.
        names: Vec<String>,
        /// The provider's reason, bounded.
        reason: String,
        /// The provider did not say which image it refused, so the newest
        /// ones were left out.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        guessed: bool,
    },
    Usage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<PromptUsage>,
        input_tokens: u64,
        output_tokens: u64,
        #[serde(default)]
        cache_read_tokens: u64,
        #[serde(default)]
        cache_write_tokens: u64,
        cost_usd: Option<f64>,
        /// The provider reported no usage for this request; the counts are
        /// the local estimate of what was sent and streamed. Such a record
        /// never anchors the context size.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        estimated: bool,
        /// Milliseconds from sending the request to its first streamed
        /// content (text, reasoning or a tool call). Absent on a request
        /// that streamed nothing, was stopped or failed, and on records made
        /// before requests were timed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        first_token_ms: Option<u64>,
        /// Milliseconds from that first content to the last, the span the
        /// output streamed across; present exactly when `first_token_ms` is.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream_ms: Option<u64>,
        /// Tokens in what streamed (text, visible reasoning, tool calls),
        /// counted with the model family's token weights; present exactly
        /// when `first_token_ms` is. Output speed divides this, not
        /// `output_tokens`, by `stream_ms`: a provider's output count also
        /// holds reasoning it never streamed, produced before the first token.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream_tokens: Option<u64>,
    },
    Todo {
        items: Vec<TodoEntry>,
    },
    /// A workspace file snapshot captured before a turn's first mutation.
    Snapshot {
        snapshot_id: String,
        file_count: u64,
        /// Transcript sequence immediately before the mutation group guarded
        /// by this snapshot. Older records omit it and use legacy inference.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        boundary_sequence: Option<u64>,
        /// Present on a completion checkpoint and points to the matching
        /// pre-mutation checkpoint. Undo applies only this bounded delta.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_snapshot_id: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FileDiffStatus {
    Added,
    Deleted,
    Modified,
}

/// Bounded file-change payload shared by Git review and snapshot/turn diffs.
/// Git responses normally carry `patch`; snapshot responses normally carry
/// `before` and `after`. A missing body with `truncated=true` remains an
/// honest metadata-only change instead of returning an unbounded payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    pub file: String,
    pub status: FileDiffStatus,
    pub additions: u64,
    pub deletions: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub binary: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// Why a reviewed file arrives without its body. `truncated` says *that* a
/// body is missing; this says *why*, so a client can tell "too big to review
/// here" from "there is nothing textual to show".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiffTruncation {
    /// Non-text content: counts only, never a patch body.
    Binary,
    /// This one file's patch exceeded the per-file review budget.
    FileTooLarge,
    /// The response's combined patch budget was already spent.
    ResponseTooLarge,
}

/// One entry of a Git review: a [`FileDiff`] plus the bounds the review had
/// to apply to produce it.
///
/// It flattens, so the wire shape is a strict superset of `FileDiff` and the
/// endpoint keeps returning the same array of the same objects; both added
/// fields are optional and defaulted, so an older client — or the generated
/// TypeScript SDK before it is regenerated — decodes these responses
/// unchanged and simply does not see them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewFile {
    #[serde(flatten)]
    pub diff: FileDiff,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<DiffTruncation>,
    /// Changed paths the response does not list at all, because the review
    /// hit its file bound. The response is an array with no envelope to hang
    /// this on, so every returned entry repeats the same count and a client
    /// renders it from whichever row it happens to hold. Absent when the
    /// review listed everything that changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted_files: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TodoEntry {
    pub content: String,
    pub status: String,
    /// Priority. Older persisted todo items did
    /// not carry it, so decoding them defaults to medium.
    #[serde(default = "default_todo_priority")]
    pub priority: String,
}

/// Versioned exact todo state carried across a lossy prose compaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TodoCheckpoint {
    pub version: u32,
    /// Sequence of the latest todo snapshot, or zero when the exact state is
    /// known to be empty without a prior todo write.
    pub revision: u64,
    pub todos: Vec<TodoEntry>,
}

fn default_todo_priority() -> String {
    "medium".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewFinding {
    pub severity: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub explanation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: ItemId,
    pub thread_id: ThreadId,
    pub turn_id: Option<TurnId>,
    pub sequence: u64,
    pub data: ItemData,
    pub created_at: DateTime<Utc>,
}

/// Unset fields resolve to configured defaults on the server, falling back to
/// the built-in `build` agent and the current default OpenAI model.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateThreadRequest {
    /// Id for the new chat, chosen by the client so a retried request cannot
    /// create a second one: when a chat with this id exists, it is returned
    /// unchanged and nothing else in the request applies. Omitted, the server
    /// chooses the id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<ScheduleOrigin>,
    #[serde(default)]
    pub title: Option<String>,
    pub parent_id: Option<ThreadId>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub ultracode: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateThreadRequest {
    pub title: Option<String>,
    pub archived: Option<bool>,
    pub agent: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub ultracode: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StartTurnRequest {
    pub request_id: RequestId,
    pub text: String,
    /// Local media paths, file URLs, or data URLs. Kept as string sources so
    /// local-first clients do not have to persist large base64 blobs.
    #[serde(default, alias = "images")]
    pub attachments: Vec<String>,
    /// Internal runtime prompt. Backward-compatible API clients omit this and
    /// receive ordinary user-message behavior.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub synthetic: bool,
    /// Non-interactive caller explicitly authorizes permission prompts for
    /// this turn. Scheduled runs set this after the schedule itself has been
    /// approved; ordinary REST/TUI clients leave it false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_approve: bool,
    pub agent: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// Per-turn tool visibility and execution policy. Missing names retain
    /// their normal availability; `false` both removes a tool from the
    /// provider schema and hard-denies a fabricated call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<HashMap<String, bool>>,
    /// Name of a custom slash command; the server expands its template with
    /// `text` as the argument string and may switch agent/model per its
    /// frontmatter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompactRequest {
    /// Caller-provided summary. When absent the server generates one with the
    /// thread's model using the hidden `compaction` agent profile.
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub files_touched: Vec<String>,
    #[serde(default)]
    pub outstanding: Vec<String>,
    /// Runtime-owned exact task state. API callers may omit this; the server
    /// always replaces it with the current durable state before committing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todo_checkpoint: Option<TodoCheckpoint>,
    /// Runtime-owned retained-tail boundary. API callers may omit this; the
    /// server computes it from the configured recent-turn count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_start_sequence: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ForkThreadRequest {
    /// Title for the fork; defaults to "Fork of {parent title}".
    pub title: Option<String>,
    /// Copy parent items up to and including this sequence; defaults to all.
    /// May accompany `before_item_id` as a compatibility fallback when it is
    /// exactly the sequence immediately before that user message.
    pub up_to_sequence: Option<u64>,
    /// Semantic fork boundary. When set, copy history strictly before this
    /// user-message item so clients can restore and edit that prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_item_id: Option<ItemId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PermissionReply {
    pub decision: PermissionReplyDecision,
    pub feedback: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QuestionReply {
    pub answer: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PermissionReplyDecision {
    Once,
    /// Allow for the rest of this server session.
    Always,
    /// Allow and persist the rule for this workspace across restarts.
    AlwaysWorkspace,
    /// Remember explicitly across linked worktrees of this Git repository.
    AlwaysRepository,
    Reject,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EventEnvelope {
    pub schema_version: String,
    pub server_instance_id: Uuid,
    pub event_id: u64,
    #[serde(rename = "type")]
    pub event_type: String,
    pub thread_id: Option<ThreadId>,
    /// Topmost ancestor of `thread_id`: equals `thread_id` for root threads,
    /// and the root of the parent chain for child-agent threads. Lets clients
    /// follow everything belonging to one root conversation.
    #[serde(default)]
    pub root_thread_id: Option<ThreadId>,
    pub turn_id: Option<TurnId>,
    pub item_id: Option<ItemId>,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    /// Effective primary agent for new conversations; absent on older servers.
    #[serde(default)]
    pub default_agent: Option<String>,
    pub name: String,
    pub version: String,
    pub schema_version: String,
    pub server_instance_id: Uuid,
    /// Latest published event id. Snapshot-first clients capture this before
    /// hydration, then replay only deltas after the snapshot boundary.
    #[serde(default)]
    pub event_cursor: u64,
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub git_branch: Option<String>,
    #[serde(default)]
    pub project_trusted: Option<bool>,
    /// Effective workspace configuration, for choosing a concrete model before
    /// creating a conversation. These are preferences, not credentials.
    #[serde(default)]
    pub default_provider: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
    /// The server's process id, so a local client that found this server in a
    /// runtime record can tell it from another process on a reused port.
    #[serde(default)]
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub authenticated: bool,
    pub models: Vec<ModelInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)] // independent provider capabilities
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    pub reasoning: bool,
    /// Concrete reasoning-effort names accepted by this model/provider pair,
    /// ordered from lightest to strongest. An empty list means callers must
    /// leave the effort unset and use the provider default.
    #[serde(default)]
    pub reasoning_efforts: Vec<String>,
    pub images: bool,
    /// Native video understanding (not video generation).
    #[serde(default)]
    pub video: bool,
    pub tools: bool,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    #[serde(default)]
    pub pricing: Option<ModelPricing>,
}

/// USD per million tokens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelPricing {
    pub input: f64,
    pub output: f64,
    #[serde(default)]
    pub cache_read: f64,
    #[serde(default)]
    pub cache_write: f64,
    /// Rates for a whole request whose prompt is longer than
    /// `aboveInputTokens` (e.g. Claude Haiku 5.5 over 100K, GPT-6 over 272K).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long_context: Option<LongContextPricing>,
}

/// USD per million tokens once a prompt exceeds `above_input_tokens`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LongContextPricing {
    pub above_input_tokens: u64,
    pub input: f64,
    pub output: f64,
    #[serde(default)]
    pub cache_read: f64,
    #[serde(default)]
    pub cache_write: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentProfile {
    pub id: String,
    pub name: String,
    pub description: String,
    pub hidden: bool,
    pub max_subagents: u8,
    /// primary | subagent | all; None = primary.
    #[serde(default)]
    pub mode: Option<String>,
    /// Pinned `provider/model` override for this agent.
    #[serde(default)]
    pub model: Option<String>,
}

impl AgentProfile {
    /// Interactive entry points exclude profiles reserved for internal work.
    pub fn selectable(&self) -> bool {
        !self.hidden && self.mode.as_deref() != Some("subagent")
    }
}

/// A recurring prompt attached to a thread: re-submitted `interval_seconds`
/// after each completed turn until `expires_at`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LoopConfig {
    pub prompt: String,
    pub interval_seconds: u64,
    pub expires_at: DateTime<Utc>,
}

/// Execution semantics shared by discovered commands and skills.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct DefinitionExecution {
    pub user_invocable: bool,
    pub disable_model_invocation: bool,
    pub fork: bool,
    pub claude_arguments: bool,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub arguments: Vec<String>,
    pub directory: Option<String>,
    pub path: Option<String>,
    pub plugin_root: Option<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
}
impl Default for DefinitionExecution {
    fn default() -> Self {
        Self {
            user_invocable: true,
            disable_model_invocation: false,
            fork: false,
            claude_arguments: false,
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            arguments: Vec::new(),
            directory: None,
            path: None,
            plugin_root: None,
            agent: None,
            model: None,
        }
    }
}

/// A user-defined slash command served from markdown files.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandInfo {
    #[serde(default)]
    pub execution: DefinitionExecution,
    pub name: String,
    pub description: String,
    /// Registry source. `workflow` identifies a dynamically discovered
    /// workflow invocation; absent values are configured prompt commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Autocomplete hint for arguments, e.g. `[file] [focus]`.
    #[serde(default)]
    pub argument_hint: Option<String>,
    /// Prompt template with `$ARGUMENTS` / `$1..$9` placeholders.
    pub template: String,
    /// Contributing plugin, when this command came from a plugin bundle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
}

/// Execute one name from the authoritative server command registry. Unlike
/// `StartTurnRequest::command`, this contract can dispatch both configured
/// prompt templates and durable workflows without overloading the stable turn
/// endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteCommandRequest {
    #[serde(default)]
    pub request_id: RequestId,
    pub name: String,
    /// Plain command tails use a JSON string. Workflow callers may supply
    /// typed JSON directly, preserving booleans/numbers/objects end to end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
    #[serde(default)]
    pub attachments: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Workflow child-agent variant (for example `high` or `xhigh`). Prompt
    /// commands use `reasoning_effort` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<HashMap<String, bool>>,
    #[serde(default)]
    pub wait: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// Tagged result of the shared command dispatcher. Prompt commands create a
/// normal turn; workflow commands return the durable run; management commands
/// such as `/workflow list` and `delete` return concise text.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandExecutionResponse {
    Turn { turn: Turn },
    WorkflowRun { run: Box<WorkflowRun> },
    Text { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub name: String,
    pub spec: String,
    /// `haarness` for installed bundles; `haarness-global` or
    /// `haarness-local` for directly loaded Haarness plugin directories, and
    /// `haarness-base` for those of the base configuration folder, which
    /// Haarness does not change (`managed` is false).
    pub source: String,
    #[serde(default)]
    pub marketplace: Option<String>,
    pub path: String,
    pub enabled: bool,
    pub managed: bool,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PluginActionRequest {
    /// install | update | enable | disable | remove | marketplace
    pub action: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillInfo {
    #[serde(default)]
    pub execution: DefinitionExecution,
    pub name: String,
    pub description: String,
    pub path: String,
    pub trusted: bool,
    pub loaded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpServerInfo {
    pub name: String,
    pub transport: String,
    pub status: String,
    pub detail: Option<String>,
    /// `base` for a server of the base configuration folder
    /// (`HAARNESS_BASE_CONFIG_DIR`), which Haarness lists but never changes:
    /// an edit saves the person's own copy. Absent for every other server and
    /// from older servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// `false` when the server's interactive apps (MCP App pages) are
    /// turned off in its configuration; absent while they are on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apps: Option<bool>,
    /// How many of the server's tools pair with an interactive page, while
    /// it is connected with its apps on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_tools: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpConfigureRequest {
    pub transport: String,
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment passed only to a local stdio MCP child process.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Working directory for a local MCP process or a dynamic header helper.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub url: Option<String>,
    /// Headers attached to every remote Streamable HTTP request.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Claude Code-compatible command that prints a JSON header object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers_helper: Option<String>,
    /// Optional connection/helper timeout. Plugin `timeout` seconds are
    /// normalized to milliseconds by configuration discovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Generic public MCP OAuth. `None` auto-detects OAuth for Streamable HTTP;
    /// `Some(false)` disables discovery for servers using another auth scheme.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<bool>,
    /// Optional public client id for servers without dynamic registration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_client_id: Option<String>,
    /// Explicit OAuth scopes. Empty lets MCP discovery select server scopes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub oauth_scopes: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Interactive apps (MCP App pages). `Some(false)` makes it a plain MCP
    /// server to Haarness: it is not told that pages render, its results
    /// carry no page and its `ui://` pages are not served. On when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apps: Option<bool>,
}
fn default_true() -> bool {
    true
}

/// One unanswered permission prompt, listable so clients can repopulate
/// pending approvals after a reconnect or thread switch — including prompts
/// raised on child-agent threads.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PendingPermissionInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_always: Option<bool>,
    /// Only a person may answer: Auto mode, `--yolo`, hooks and configured
    /// rules never do, so clients must not answer it automatically. Its
    /// "always" lasts for this chat only (a Chrome site approval).
    #[serde(default)]
    pub explicit: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_scope: Option<String>,
    pub permission_id: String,
    pub operation: String,
    #[serde(default)]
    pub detail: Value,
    /// For any request a shell command raises (`bash`, and under the
    /// default rules `external_directory`), the script the command runs
    /// (`bash x.sh`, `./x.sh`, `source ./env.sh`; a `source` name without a
    /// slash is not claimed), read when it asked, for the person who
    /// answers. Display only: it changes no pattern or rule. An Allow once
    /// does not run the command if the file is no longer what was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<PermissionScript>,
    /// Permission category (bash, edit, external_directory, …).
    #[serde(default)]
    pub permission: String,
    /// Exact things being asked about (command texts, paths, globs).
    #[serde(default)]
    pub patterns: Vec<String>,
    /// Rules an "always" answer would persist, e.g. `bash:git status *`.
    #[serde(default)]
    pub always_patterns: Vec<String>,
    pub thread_id: ThreadId,
    #[serde(default)]
    pub root_thread_id: Option<ThreadId>,
    #[serde(default)]
    pub turn_id: Option<TurnId>,
    #[serde(default)]
    pub agent: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// The script file a shell command runs, as the server read it when the
/// command asked for permission.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PermissionScript {
    /// The file the command names, as an absolute path (links are followed
    /// to read it, not here), with the replacements `content` has and its
    /// tabs and line breaks as `�` too.
    pub path: String,
    /// Its text: at most its first 30,000 bytes, with line breaks as `\n`
    /// and other control characters (tabs aside), bidirectional controls
    /// and marks, and Unicode line and paragraph separators shown as `�`.
    pub content: String,
    /// The file goes on past `content`.
    #[serde(default)]
    pub truncated: bool,
    /// Why the text is not shown (`content` is empty, and a card names the
    /// file instead): what runs may not be what is on disk now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withheld: Option<ScriptWithheld>,
}

/// Why a script's text is not shown on its card.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScriptWithheld {
    /// The command writes the file before it runs it (`curl -o x.sh … &&
    /// bash x.sh`, `cat > x.sh <<EOF`).
    Written,
    /// An earlier step of the command could change it: a program not known
    /// to leave files alone (`make`, `git`, `bash -c …`, `eval`, a function).
    Changeable,
}

/// One unanswered model question, listable so clients can hydrate every
/// prompt in a root conversation after reconnecting or switching threads.
/// Questions raised by parallel child agents remain distinct queue entries.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PendingQuestionInfo {
    pub question_id: String,
    pub prompt: String,
    #[serde(default)]
    pub choices: Vec<String>,
    #[serde(default)]
    pub multiple: bool,
    #[serde(default)]
    pub custom: bool,
    pub thread_id: ThreadId,
    #[serde(default)]
    pub root_thread_id: Option<ThreadId>,
    #[serde(default)]
    pub turn_id: Option<TurnId>,
    pub created_at: DateTime<Utc>,
}

/// Process-local child-agent work associated with one parent conversation.
/// Child threads and their transcript are durable; the live/background flag
/// intentionally is not and disappears when the server process stops.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTaskInfo {
    pub parent_thread_id: ThreadId,
    pub parent_turn_id: TurnId,
    pub call_id: String,
    pub child_thread_id: ThreadId,
    pub child_turn_id: TurnId,
    pub agent: String,
    pub description: String,
    pub background: bool,
    pub started_at: DateTime<Utc>,
}

/// What a chat is doing now (`GET /v1/threads/{id}/activity`). Work the chat
/// started in the background (child agents launched or moved there, workflow
/// runs started without waiting) delivers its result into the chat when it
/// ends, starting a new turn when the chat is idle. A chat is quiet only when
/// no turn runs in it and `background` is 0. `turn.finished` events carry the
/// same count, as of the moment the turn finished, under the same name.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ThreadActivity {
    /// The turn running in this chat, if any. Always sent, null when none.
    #[schema(required = true)]
    pub active_turn: Option<Turn>,
    /// Background results this chat is still owed.
    pub background: u32,
    /// The chat's newest turn by creation, whatever its status; null for a
    /// chat without turns. Read after the other two, so a turn that started
    /// and ended since an earlier read shows here. Always sent; servers
    /// before 2026-10-17 leave it out.
    #[serde(default)]
    #[schema(required = true)]
    pub last_turn: Option<Turn>,
}

/// User input against a thread that may already have a running turn. When a
/// turn is active the text is persisted immediately and steered into that
/// turn's next provider round; otherwise a normal turn starts.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueInputRequest {
    /// Whether the submitting client can show sandboxed file previews. Text
    /// clients say `false`; `None` keeps the chat's recorded value so a
    /// phone or a background result does not change the tools or prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<bool>,
    /// Reasoning effort for a turn this input starts. `None` keeps the
    /// effort of the chat's latest prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Steer by default for compatibility; queue waits for an idle boundary.
    #[serde(default)]
    pub delivery: InputDelivery,
    pub request_id: RequestId,
    pub text: String,
    #[serde(default, alias = "images")]
    pub attachments: Vec<String>,
    /// Internal runtime prompt, hidden from user-authored transcript/history
    /// surfaces but still delivered to the provider.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub synthetic: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueInputResponse {
    /// True when the message was queued into an already-running turn.
    pub queued: bool,
    /// The queued user-message item (queued == true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<Item>,
    /// The freshly started turn (queued == false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<Turn>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

#[derive(OpenApi)]
#[openapi(
    info(title = "Haarness API", version = "1"),
    components(schemas(
        Thread,
        Turn,
        Item,
        ItemData,
        ReviewFinding,
        TodoEntry,
        RevertState,
        FileDiff,
        FileDiffStatus,
        DiffTruncation,
        ReviewFile,
        CommandInfo,
        ExecuteCommandRequest,
        CommandExecutionResponse,
        PluginInfo,
        PluginActionRequest,
        CreateThreadRequest,
        UpdateThreadRequest,
        StartTurnRequest,
        CompactRequest,
        ForkThreadRequest,
        PermissionReply,
        QuestionReply,
        PermissionReplyDecision,
        ApprovalMode,
        ApprovalModeSetting,
        ApprovalModeInfo,
        SetApprovalModeRequest,
        EventEnvelope,
        ServerInfo,
        ProviderInfo,
        ModelInfo,
        ModelPricing,
        LongContextPricing,
        AgentProfile,
        SkillInfo,
        McpServerInfo,
        McpConfigureRequest,
        LoopConfig,
        PendingPermissionInfo,
        PermissionScript,
        ScriptWithheld,
        PendingQuestionInfo,
        BackgroundTaskInfo,
        ThreadActivity,
        InputDelivery,
        PendingInput,
        InputQueue,
        InputQueueAction,
        QueueInputRequest,
        QueueInputResponse,
        ApiError,
        TurnStatus,
        WorkflowArgument,
        WorkflowArgumentType,
        WorkflowDefinition,
        WorkflowDialect,
        WorkflowFinding,
        WorkflowId,
        WorkflowInvalidDefinition,
        WorkflowListResponse,
        WorkflowLog,
        WorkflowLogSource,
        WorkflowMeta,
        WorkflowPhase,
        WorkflowRun,
        WorkflowRunAgent,
        WorkflowRunAgentStatus,
        WorkflowRunDefinition,
        WorkflowRunStatus,
        WorkflowRunTime,
        WorkflowRunWarning,
        WorkflowStartRequest,
        WorkflowSourceResponse,
        GoalStatus,
        GoalVerdict,
        GoalCriterion,
        GoalCompletionOption,
        GoalCompletion,
        GoalState,
        GoalActionRequest,
        GoalActionResponse,
        GoalCriteriaEvidence,
        GoalUpdateStatus,
        GoalUpdateRequest,
        GoalUpdatedEvent,
        GoalLogicalTurnPhase,
        GoalLogicalTurnEvent,
        ProjectInfo,
        WorkspaceKind,
        WorkspaceInfo,
        WorkspaceListResponse,
        WorkspaceRefKind,
        WorkspaceRefInfo,
        WorkspaceRefsResponse,
        WorkspaceResolveRefRequest,
        WorkspaceResolveRefResponse,
        WorkspaceCreateRequest,
        WorkspaceResetRequest,
        WorkspaceRemoveRequest,
        WorkspaceCleanupRequest,
        WorkspaceOperationResponse,
        LazyToolMode,
        ToolCatalogEntry,
        ToolCatalogResponse,
        ToolSettingsUpdate,
        LazyToolSettingsUpdate,
        BtwRequest,
        BtwTurn,
        BtwResponse,
        BtwEvent,
        PtyId,
        PtyStatus,
        PtyInfo,
        PtyCreateRequest,
        PtyUpdateRequest,
        PtyCursor
    ))
)]
pub struct ApiDoc;

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        CommandExecutionResponse, CompactRequest, ExecuteCommandRequest, ItemData, ModelInfo,
        StartTurnRequest,
    };

    #[test]
    fn command_execution_contract_is_tagged_and_defaults_request_metadata() {
        let request: ExecuteCommandRequest =
            serde_json::from_value(json!({"name": "release"})).unwrap();
        assert_eq!(request.name, "release");
        assert!(request.arguments.is_none());
        assert!(!request.wait);

        let response = CommandExecutionResponse::Text {
            text: "listed".into(),
        };
        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({"kind": "text", "text": "listed"})
        );
    }

    #[test]
    fn legacy_compact_requests_with_a_string_summary_still_parse() {
        let request: CompactRequest =
            serde_json::from_value(json!({"summary": "what happened"})).unwrap();
        assert_eq!(request.summary.as_deref(), Some("what happened"));

        let request: CompactRequest = serde_json::from_value(json!({})).unwrap();
        assert!(request.summary.is_none());
    }

    #[test]
    fn workspace_cleanup_request_has_an_exact_camel_case_wire_contract() {
        let request: super::WorkspaceCleanupRequest =
            serde_json::from_value(json!({"confirm": true})).unwrap();
        assert!(request.confirm);
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({"confirm": true})
        );
        let default: super::WorkspaceCleanupRequest = serde_json::from_value(json!({})).unwrap();
        assert!(!default.confirm);
    }

    #[test]
    fn legacy_create_thread_requests_with_concrete_strings_still_parse() {
        let request: super::CreateThreadRequest = serde_json::from_value(json!({
            "title": "My thread",
            "agent": "build",
            "provider": "openai",
            "model": "gpt-5.4"
        }))
        .unwrap();
        assert_eq!(request.title.as_deref(), Some("My thread"));
        assert_eq!(request.provider.as_deref(), Some("openai"));
        assert!(request.ultracode.is_none());

        let empty: super::CreateThreadRequest = serde_json::from_value(json!({})).unwrap();
        assert!(empty.title.is_none());
        assert!(empty.provider.is_none());
        assert!(empty.ultracode.is_none());
    }

    #[test]
    fn legacy_start_turn_requests_without_reasoning_effort_still_parse() {
        let request: StartTurnRequest = serde_json::from_value(json!({
            "requestId": uuid::Uuid::now_v7(),
            "text": "hello",
            "images": ["/tmp/legacy.png"]
        }))
        .unwrap();
        assert!(request.reasoning_effort.is_none());
        assert!(request.tools.is_none());
        assert_eq!(request.attachments, ["/tmp/legacy.png"]);

        let request: StartTurnRequest = serde_json::from_value(json!({
            "requestId": uuid::Uuid::now_v7(),
            "text": "bounded",
            "tools": {"bash": false, "read": true}
        }))
        .unwrap();
        let tools = request.tools.unwrap();
        assert_eq!(tools.get("bash"), Some(&false));
        assert_eq!(tools.get("read"), Some(&true));
    }

    #[test]
    fn legacy_model_info_without_metadata_still_parses() {
        let info: ModelInfo = serde_json::from_value(json!({
            "id": "echo-1",
            "displayName": "Echo",
            "reasoning": false,
            "images": false,
            "tools": true
        }))
        .unwrap();
        assert!(info.context_window.is_none());
        assert!(info.pricing.is_none());
        assert!(info.reasoning_efforts.is_empty());
    }

    #[test]
    fn legacy_threads_and_turns_default_ultracode_off() {
        let thread: super::Thread = serde_json::from_value(json!({
            "id": uuid::Uuid::now_v7(),
            "title": "Legacy",
            "parentId": null,
            "archived": false,
            "agent": "build",
            "provider": "openai",
            "model": "gpt-5.4",
            "createdAt": "2026-08-24T00:00:00Z",
            "updatedAt": "2026-08-24T00:00:00Z"
        }))
        .unwrap();
        assert!(!thread.ultracode);

        let turn: super::Turn = serde_json::from_value(json!({
            "id": uuid::Uuid::now_v7(),
            "threadId": thread.id,
            "requestId": uuid::Uuid::now_v7(),
            "status": "completed",
            "agent": "build",
            "provider": "openai",
            "model": "gpt-5.4",
            "error": null,
            "createdAt": "2026-08-24T00:00:00Z",
            "updatedAt": "2026-08-24T00:00:00Z"
        }))
        .unwrap();
        assert!(!turn.ultracode);
    }

    #[test]
    fn legacy_usage_items_without_cache_fields_still_parse() {
        let item: ItemData = serde_json::from_value(json!({
            "type": "usage",
            "input_tokens": 10,
            "output_tokens": 5,
            "cost_usd": null
        }))
        .unwrap();
        assert!(matches!(
            item,
            ItemData::Usage {
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                ..
            }
        ));
    }

    #[test]
    fn usage_timing_is_left_out_until_a_request_was_timed() {
        let untimed: ItemData = serde_json::from_value(json!({
            "type": "usage",
            "input_tokens": 10,
            "output_tokens": 5,
            "cost_usd": null
        }))
        .unwrap();
        assert!(matches!(
            untimed,
            ItemData::Usage {
                first_token_ms: None,
                stream_ms: None,
                ..
            }
        ));
        let written = serde_json::to_value(&untimed).unwrap();
        assert!(written.get("first_token_ms").is_none(), "{written}");
        assert!(written.get("stream_ms").is_none(), "{written}");
        let timed: ItemData = serde_json::from_value(json!({
            "type": "usage",
            "input_tokens": 10,
            "output_tokens": 5,
            "cost_usd": null,
            "first_token_ms": 820,
            "stream_ms": 2400
        }))
        .unwrap();
        let written = serde_json::to_value(&timed).unwrap();
        assert_eq!(
            (
                written["first_token_ms"].as_u64(),
                written["stream_ms"].as_u64()
            ),
            (Some(820), Some(2400))
        );
    }

    #[test]
    fn old_tool_calls_deserialize_without_provider_metadata() {
        let item: ItemData = serde_json::from_value(json!({
            "type": "tool_call",
            "call_id": "call_1",
            "name": "list",
            "arguments": {"path": "."}
        }))
        .unwrap();

        assert!(matches!(
            item,
            ItemData::ToolCall { provider_metadata, .. }
                if provider_metadata == Value::Null
        ));
    }
}
