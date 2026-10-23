use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

use async_trait::async_trait;
use futures::Stream;
use haarness_protocol::{
    CompactRequest, CreateThreadRequest, GoalState, Item, ItemData, ItemId, McpConfigureRequest,
    McpServerInfo, ModelInfo, ProviderInfo, SessionArchive, SessionArchiveImportResult, SkillInfo,
    StartTurnRequest, Thread, ThreadId, Turn, TurnId, TurnStatus, UpdateThreadRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Error, Clone)]
pub enum CoreError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    /// The user rejected an approval prompt (vs. a configured rule denial):
    /// the model should adjust course, not treat it as policy.
    #[error("{0}")]
    PermissionRejected(String),
    /// Auto's reviewer refused the call. The model reads why, and that it
    /// should not reach the same result another way.
    #[error("{}", crate::auto_review::blocked_message(.0))]
    ReviewBlocked(Box<haarness_protocol::PermissionReview>),
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("provider error: {0}")]
    Provider(String),
    /// The provider's safety check (a content filter, a guardrail or the
    /// model's own refusal) stopped the response. Never retried.
    #[error(
        "The provider's safety check stopped this response ({0}). Send a follow-up message to try again; if it stops again, rephrase the request or choose another model."
    )]
    ProviderBlocked(String),
    #[error("Provider response ended before completion ({0}).")]
    ProviderIncomplete(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("tool error: {0}")]
    Tool(String),
    #[error("operation cancelled")]
    Cancelled,
}

impl CoreError {
    /// A short, stable category for telemetry (`error.type` on spans). Never the
    /// message: provider and tool messages can carry response bodies and paths.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "not_found",
            Self::Conflict(_) => "conflict",
            Self::PermissionDenied(_) => "permission_denied",
            Self::PermissionRejected(_) => "permission_rejected",
            Self::ReviewBlocked(_) => "review_blocked",
            Self::Invalid(_) => "invalid_request",
            Self::Provider(_) => "provider_error",
            Self::ProviderBlocked(_) => "provider_blocked",
            Self::ProviderIncomplete(_) => "provider_incomplete",
            Self::Storage(_) => "storage_error",
            Self::Tool(_) => "tool_error",
            Self::Cancelled => "cancelled",
        }
    }
}

pub type CoreResult<T> = Result<T, CoreError>;

/// A subagent launched (or moved) into the background whose run ended
/// without its result reaching the chat that launched it: the server
/// stopped while it ran, or stopped between its end and the delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrandedTask {
    pub parent_thread_id: ThreadId,
    /// The turn that launched it, which holds its task checkpoints.
    pub parent_turn_id: Option<TurnId>,
    pub call_id: String,
    pub child_thread_id: ThreadId,
    pub child_turn_id: TurnId,
    /// The task's state as recovery recorded it: `done`, `failed` or
    /// `interrupted`.
    pub state: String,
    /// The `task` call's description, or the subagent chat's title.
    pub description: String,
    /// The subagent's last reply in that run, when it gave one.
    pub reply: Option<String>,
    /// Why its run failed, when it did.
    pub error: Option<String>,
}

/// What recovery records in the chat that launched a [`StrandedTask`].
pub type StrandedReport<'a> = dyn Fn(&StrandedTask) -> Vec<ItemData> + Send + Sync + 'a;

#[async_trait]
pub trait ThreadRepository: Send + Sync {
    /// Export one root thread and its complete descendant tree as a portable
    /// archive. Production repositories should provide a consistent snapshot.
    async fn export_session_archive(
        &self,
        _id: ThreadId,
        _sanitize: bool,
    ) -> CoreResult<SessionArchive> {
        Err(CoreError::Storage(
            "session archive export is unavailable".into(),
        ))
    }
    /// Restore a portable archive while preserving stable thread, turn, and
    /// item IDs. Re-importing the same archive must be idempotent.
    async fn import_session_archive(
        &self,
        _archive: SessionArchive,
    ) -> CoreResult<SessionArchiveImportResult> {
        Err(CoreError::Storage(
            "session archive import is unavailable".into(),
        ))
    }
    async fn create_thread(&self, request: CreateThreadRequest) -> CoreResult<Thread>;
    async fn list_threads(&self, include_archived: bool) -> CoreResult<Vec<Thread>>;
    async fn get_thread(&self, id: ThreadId) -> CoreResult<Thread>;
    async fn update_thread(&self, id: ThreadId, request: UpdateThreadRequest)
    -> CoreResult<Thread>;
    /// Atomically apply a generated title only while the user-visible title
    /// still matches. Stores without conditional writes may skip auto-titling.
    async fn update_generated_title(
        &self,
        _id: ThreadId,
        _expected: &str,
        _title: &str,
    ) -> CoreResult<Option<Thread>> {
        Ok(None)
    }
    async fn has_input_admission(
        &self,
        _id: ThreadId,
        _request: haarness_protocol::RequestId,
    ) -> CoreResult<bool> {
        Ok(false)
    }
    async fn input_queue(&self, _id: ThreadId) -> CoreResult<haarness_protocol::InputQueue> {
        Ok(Default::default())
    }
    async fn enqueue_input(
        &self,
        _id: ThreadId,
        _input: haarness_protocol::PendingInput,
    ) -> CoreResult<haarness_protocol::InputQueue> {
        Err(CoreError::Invalid("Deferred input is unavailable".into()))
    }
    async fn change_input_queue(
        &self,
        _id: ThreadId,
        _action: haarness_protocol::InputQueueAction,
    ) -> CoreResult<haarness_protocol::InputQueue> {
        Err(CoreError::Invalid("Deferred input is unavailable".into()))
    }
    /// Atomically remove a pending input and append its visible user message.
    /// None selects the first input; an explicit id/revision promotes a steer.
    async fn promote_input(
        &self,
        _id: ThreadId,
        _turn: TurnId,
        _selected: Option<(haarness_protocol::RequestId, u64)>,
    ) -> CoreResult<Option<Item>> {
        Ok(None)
    }
    async fn list_items(&self, id: ThreadId, after: Option<u64>) -> CoreResult<Vec<Item>>;
    async fn start_turn(
        &self,
        id: ThreadId,
        request: StartTurnRequest,
    ) -> CoreResult<(Turn, Item, bool)>;
    async fn start_queued_turn(
        &self,
        _id: ThreadId,
        _request: StartTurnRequest,
        _revision: u64,
    ) -> CoreResult<(Turn, Item, bool)> {
        Err(CoreError::Invalid("Deferred input is unavailable".into()))
    }
    async fn get_turn(&self, id: TurnId) -> CoreResult<Turn>;
    /// The thread's newest turn by creation, whatever its status. Stores that
    /// cannot say report none.
    async fn last_turn(&self, _id: ThreadId) -> CoreResult<Option<Turn>> {
        Ok(None)
    }
    async fn prepare_command_input(
        &self,
        _thread: ThreadId,
        _turn: TurnId,
        _text: String,
    ) -> CoreResult<Item> {
        Err(CoreError::Storage(
            "command input preparation unavailable".into(),
        ))
    }
    async fn append_item(
        &self,
        thread: ThreadId,
        turn: Option<TurnId>,
        data: ItemData,
    ) -> CoreResult<Item>;
    /// Append `items` to `thread` in order, all or none. Stores without
    /// transactions append them one at a time.
    async fn append_items(
        &self,
        thread: ThreadId,
        turn: Option<TurnId>,
        items: Vec<ItemData>,
    ) -> CoreResult<Vec<Item>> {
        let mut appended = Vec::with_capacity(items.len());
        for data in items {
            appended.push(self.append_item(thread, turn, data).await?);
        }
        Ok(appended)
    }
    async fn set_turn_status(
        &self,
        id: TurnId,
        status: TurnStatus,
        error: Option<String>,
    ) -> CoreResult<Turn>;
    /// Append a compaction boundary only when the current visible tail still
    /// equals `expected_tail`. `None` means the caller observed an empty
    /// transcript. The comparison and append must be one storage transaction.
    async fn compact(
        &self,
        id: ThreadId,
        request: CompactRequest,
        expected_tail: Option<(ItemId, u64)>,
    ) -> CoreResult<Item>;
    /// Create a new thread carrying the parent's agent/provider/model and a
    /// copy of its items up to and including `up_to_sequence` (all when
    /// `None`). Copied items keep their sequence but detach from turns.
    async fn fork_thread(
        &self,
        id: ThreadId,
        title: Option<String>,
        up_to_sequence: Option<u64>,
    ) -> CoreResult<Thread>;
    /// Record or clear the thread's reverted state (sequence, pre-revert
    /// backup snapshot id, post-undo/base snapshot id).
    async fn set_revert_state(
        &self,
        id: ThreadId,
        state: Option<(u64, String, Option<String>)>,
    ) -> CoreResult<()>;
    /// Commit a pending revert before a direct host-side action. Production
    /// repositories override this with one transaction; the default keeps
    /// test adapters source-compatible.
    async fn confirm_revert(&self, id: ThreadId) -> CoreResult<bool> {
        let thread = self.get_thread(id).await?;
        let Some(revert) = thread.revert else {
            return Ok(false);
        };
        self.soft_delete_items_after(id, revert.sequence).await?;
        self.set_revert_state(id, None).await?;
        Ok(true)
    }
    /// Soft-delete items past `sequence`; they stay in the database but leave
    /// every listing. Returns the number hidden.
    async fn soft_delete_items_after(&self, id: ThreadId, sequence: u64) -> CoreResult<u64>;
    /// Persist a workspace-scoped permission approval across restarts.
    async fn save_permission_rule(
        &self,
        workspace: &str,
        rule: &crate::PermissionRule,
    ) -> CoreResult<()>;
    async fn load_permission_rules(
        &self,
        workspace: &str,
    ) -> CoreResult<Vec<crate::PermissionRule>>;
    async fn recover_interrupted(&self) -> CoreResult<u64>;
    /// [`Self::recover_interrupted`], also recording in each launching chat
    /// the background subagents whose result it is still owed: `report`'s
    /// items are written with the task's final checkpoint, in one step, so a
    /// crash or failure cannot record one without the other. A chat that
    /// already holds the result, or is archived, gets none. Returns the items
    /// written. A repository that does not track task checkpoints records
    /// none.
    async fn recover_interrupted_tasks(
        &self,
        _report: &StrandedReport<'_>,
    ) -> CoreResult<(u64, Vec<Item>)> {
        Ok((self.recover_interrupted().await?, Vec::new()))
    }
    /// Standing goal + loop configuration for a thread (absent by default).
    async fn thread_extras(
        &self,
        id: ThreadId,
    ) -> CoreResult<(Option<String>, Option<haarness_protocol::LoopConfig>)>;
    async fn set_goal(&self, id: ThreadId, goal: Option<String>) -> CoreResult<()>;
    /// Read the canonical whole-state goal document. Implementers must
    /// preserve its stable goal ID and lifecycle/accounting fields; deriving
    /// a fresh state from the legacy text column on every read is unsafe.
    async fn goal_state(&self, id: ThreadId) -> CoreResult<Option<GoalState>>;
    /// Persist or clear the canonical whole-state goal document.
    async fn save_goal_state(&self, id: ThreadId, state: Option<GoalState>) -> CoreResult<()>;
    async fn set_loop(
        &self,
        id: ThreadId,
        config: Option<haarness_protocol::LoopConfig>,
    ) -> CoreResult<()>;

    /// Read one workspace-scoped runtime setting. The defaults keep custom
    /// repository adapters source-compatible; production storage persists it.
    async fn runtime_setting(&self, _workspace: &str, _key: &str) -> CoreResult<Option<Value>> {
        Ok(None)
    }
    /// Persist one workspace-scoped runtime setting. `None` removes it.
    async fn save_runtime_setting(
        &self,
        _workspace: &str,
        _key: &str,
        _value: Option<Value>,
    ) -> CoreResult<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderMessage {
    Text {
        role: String,
        content: String,
    },
    /// A user message containing local or inline media. Providers translate
    /// the sources into their native multimodal content blocks.
    Media {
        role: String,
        content: String,
        attachments: Vec<String>,
    },
    ToolCall {
        call_id: String,
        name: String,
        arguments: Value,
        provider_metadata: Value,
    },
    ToolResult {
        call_id: String,
        name: String,
        output: String,
        is_error: bool,
        /// Binary tool output that must remain provider-visible without
        /// expanding base64 into the textual result. Sources use the same
        /// local-path/file-URL/data-URL contract as user attachments.
        #[serde(default)]
        attachments: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Reserved provider-neutral tool used for schema-constrained final answers.
///
/// Providers expose this as a required function/tool call while the runtime
/// intercepts its arguments as the final assistant value. It deliberately
/// bypasses the ordinary tool registry: it is response framing, not workspace
/// authority.
pub const STRUCTURED_OUTPUT_TOOL_NAME: &str = "StructuredOutput";

#[must_use]
pub fn structured_output_tool(schema: &Value) -> ToolDefinition {
    let mut input_schema = schema.clone();
    if let Some(object) = input_schema.as_object_mut() {
        // Provider tool schemas do not need (and Gemini rejects) the draft URI.
        object.remove("$schema");
    }
    ToolDefinition {
        name: STRUCTURED_OUTPUT_TOOL_NAME.into(),
        description: "Submit the final response as JSON matching the required schema. Call this only when the answer is complete.".into(),
        input_schema,
    }
}

#[derive(Debug, Clone)]
pub struct ProviderRequest {
    pub model: String,
    pub system: String,
    pub messages: Vec<ProviderMessage>,
    pub tools: Vec<ToolDefinition>,
    pub reasoning_effort: Option<String>,
    /// When present, providers must offer `StructuredOutput` and require a
    /// tool call. A successful call is emitted as `ProviderEvent::StructuredOutput`
    /// instead of entering ordinary tool execution.
    pub output_schema: Option<Value>,
    /// Sampling limits and transport extras chosen by request hooks
    /// (`chat.params`, `chat.headers`). Providers apply what their wire
    /// protocol supports and keep their defaults otherwise.
    pub parameters: RequestParameters,
}

/// Provider-independent request knobs. `None` keeps the provider default.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RequestParameters {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<u32>,
    pub max_output_tokens: Option<u64>,
    /// Extra HTTP headers for HTTP transports (OpenAI-compatible, Anthropic,
    /// Gemini and OpenAI). Signed transports such as Bedrock ignore them.
    pub headers: BTreeMap<String, String>,
    /// Extra JSON body fields merged into OpenAI-compatible requests.
    pub options: serde_json::Map<String, Value>,
    /// Stable id of the conversation. `OpenAI` sends it as `prompt_cache_key`:
    /// without it the `ChatGPT` backend caches nothing between rounds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_key: Option<String>,
    /// How long the provider may stay silent, from its configuration. Not part
    /// of the request on the wire.
    #[serde(skip)]
    pub limits: crate::StreamLimits,
    /// No later request reads this one's prefix (a compaction summary):
    /// providers that mark prompt-cache breakpoints leave them out, so no
    /// cache entry is written and paid for. Not part of the request on the
    /// wire.
    #[serde(skip)]
    pub uncached: bool,
}

#[derive(Debug, Clone)]
pub enum ProviderEvent {
    TextDelta(String),
    ReasoningDelta(String),
    ToolCall {
        call_id: String,
        name: String,
        arguments: Value,
        provider_metadata: Value,
    },
    StructuredOutput(Value),
    /// The response began content the adapter holds back until it is whole:
    /// a tool call's arguments, or structured output. Only the request's
    /// first-token clock reads it; nothing is stored or shown, and it does
    /// not count as an answer for the retry rules.
    ContentStarted,
    /// One request's token counts. `input_tokens` is the whole prompt, cache reads and writes included (`OpenAI`'s
    /// convention; Anthropic and Bedrock adapters add their separate cache counters in), so it measures the context
    /// for compaction and the context meter. `cache_read_tokens` and `cache_write_tokens` say how much of it was
    /// read from or written to the provider's cache, for pricing.
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_write_tokens: u64,
    },
    Finished {
        reason: String,
    },
}

impl ProviderEvent {
    /// Classify incomplete and failed responses while retaining ordinary
    /// stop/length/tool finishes. Policy blocks must never trigger retries:
    /// Anthropic's `refusal`, Bedrock's `content_filtered` and
    /// `guardrail_intervened`, and the content filters of the others.
    pub fn finish_error(reason: &str) -> Option<CoreError> {
        let normalized = reason.to_ascii_lowercase().replace('-', "_");
        match normalized.as_str() {
            "content_filter"
            | "content_filtered"
            | "guardrail_intervened"
            | "refusal"
            | "safety"
            | "recitation"
            | "blocklist"
            | "prohibited_content"
            | "spii"
            | "image_safety"
            | "image_prohibited_content" => Some(CoreError::ProviderBlocked(reason.to_owned())),
            // Gemini's dropped calls (see `dropped_tool_call`) are incomplete
            // responses too, not finished ones with an empty answer.
            ""
            | "unknown"
            | "other"
            | "incomplete"
            | "malformed_function_call"
            | "unexpected_tool_call" => Some(CoreError::ProviderIncomplete(reason.to_owned())),
            "error" | "failed" => Some(CoreError::Provider(
                "provider ended the response with an error".into(),
            )),
            _ => None,
        }
    }

    /// Gemini drops a function call it cannot parse, or one naming a tool it
    /// was not offered, and ends the response with `MALFORMED_FUNCTION_CALL`
    /// or `UNEXPECTED_TOOL_CALL`, often right after valid parallel calls. The
    /// response is incomplete rather than failed: the valid calls can run and
    /// the model can issue the dropped one again.
    pub fn dropped_tool_call(reason: &str) -> bool {
        matches!(
            reason.to_ascii_lowercase().replace('-', "_").as_str(),
            "malformed_function_call" | "unexpected_tool_call"
        )
    }
}

pub type ProviderStream = Pin<Box<dyn Stream<Item = CoreResult<ProviderEvent>> + Send>>;

/// What a provider said about an API key that is about to be stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyCheck {
    /// The provider accepted the key.
    Accepted,
    /// The provider refused the key, with its own reason when it gave one.
    Rejected(Option<String>),
    /// The key could not be checked: the provider was unreachable, slow, a
    /// gateway in front of it refused the check, or it failed for another
    /// reason. Says why, in a few words.
    Unchecked(String),
    /// The provider has no cheap request that tells.
    Unsupported,
}

#[async_trait]
pub trait ModelProvider: Send + Sync {
    fn id(&self) -> &str;
    fn info(&self) -> ProviderInfo;
    /// Keep provider-specific schemas while dropping the requirement that a
    /// side question produce structured output. This grants no tool execution.
    fn prepare_side_question(&self, request: &mut ProviderRequest) {
        if let Some(schema) = request.output_schema.take() {
            let tool = structured_output_tool(&schema);
            request.tools.retain(|entry| entry.name != tool.name);
            request.tools.push(tool);
        }
    }
    /// Revalidate model availability and metadata without interrupting active requests.
    async fn refresh_models(&self, _force: bool) {}
    /// Ask the provider whether it accepts `key`, without storing or using it
    /// for anything else: one cheap authenticated request, where there is one.
    async fn check_api_key(&self, _key: &str) -> KeyCheck {
        KeyCheck::Unsupported
    }
    /// Who can read the prompt cache for `model`, when this provider writes
    /// it explicitly (Anthropic `cache_control` breakpoints). Such an entry is
    /// readable only once the response to the request that wrote it begins,
    /// so the runtime holds back concurrent sibling requests until then. The
    /// value fingerprints the endpoint and credentials and must not contain a
    /// secret. `None`, the default: the provider caches automatically or not
    /// at all.
    fn explicit_prompt_cache_scope(&self, _model: &str) -> Option<String> {
        None
    }
    async fn stream(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
    ) -> CoreResult<ProviderStream>;
}

pub trait ProviderRegistry: Send + Sync {
    fn get(&self, id: &str) -> Option<Arc<dyn ModelProvider>>;
    fn list(&self) -> Vec<ProviderInfo>;
    fn models(&self, provider: &str) -> Vec<ModelInfo> {
        self.get(provider)
            .map_or_else(Vec::new, |provider| provider.info().models)
    }
}

#[derive(Clone)]
pub struct ToolContext {
    pub execution: crate::ExecutionSettings,
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
    pub workspace: PathBuf,
    /// Exact canonical external paths authorized for this one tool call.
    /// The runtime populates this only after `external_directory` resolves to
    /// allow (by rule or by an interactive once/always reply).
    pub authorized_external_paths: Vec<PathBuf>,
    pub cancel: CancellationToken,
    /// Re-enter the active turn's ordinary tool boundary. Composite tools use
    /// this instead of calling a registry directly, so hooks, permissions,
    /// snapshots, cancellation, and workspace availability remain in force.
    pub invoker: Option<Arc<dyn ToolInvoker>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub text: String,
    pub metadata: Value,
}

#[async_trait]
pub trait ToolInvoker: Send + Sync {
    fn parallel_safe(&self, _name: &str, _arguments: &Value) -> bool {
        false
    }
    fn activate_skill_permissions(&self, _execution: &haarness_protocol::DefinitionExecution) {}
    fn workspace(&self) -> Option<String> {
        None
    }
    /// Agents a command template may hand work to through `@name`: visible
    /// custom agents whose mode is `subagent` or `all`.
    fn delegable_agents(&self) -> Vec<String> {
        Vec::new()
    }
    /// Resolve a provider namespace before policy checks or composite planning.
    fn resolve_name(&self, name: &str) -> String {
        name.to_owned()
    }
    /// The rules this call's agent reads files under, so a search can leave
    /// out a file a `read` rule denies instead of returning its lines or name.
    fn read_rules(&self) -> Option<crate::PermissionEngine> {
        None
    }
    async fn invoke(&self, name: &str, arguments: Value) -> CoreResult<ToolOutput>;
    /// A batch step refused before it starts still takes its number, so the
    /// call ids of the steps after it (`<batch call id>:N`) stay their place
    /// in the batch, which is how clients pair a delegation with its step.
    fn skip_call(&self) {}
    /// Runs a definition's `!` snippet in the agent's shell. Permissions,
    /// hooks and the approval card see `shown`, the snippet with its folders
    /// filled in; the shell runs `run`, which keeps the placeholders as
    /// written, with `env` (the values they stand for) in its environment.
    /// An invoker without its own shell runs `shown`.
    async fn definition_shell(
        &self,
        shown: String,
        _run: String,
        _env: std::collections::BTreeMap<String, String>,
    ) -> CoreResult<ToolOutput> {
        self.invoke("bash", serde_json::json!({"command":shown}))
            .await
    }
    fn side_effectful_for(&self, name: &str, arguments: &Value) -> CoreResult<bool>;
}

#[async_trait]
pub trait Tool: Send + Sync {
    /// Opt-in for observations that can run beside other observations. Local
    /// snapshot classification alone is insufficient (MCP may mutate remotely).
    fn parallel_safe(&self, _arguments: &Value) -> bool {
        false
    }
    fn definition(&self) -> ToolDefinition;
    /// Execution contract. A native tool may retain documented legacy inputs
    /// while advertising only the preferred shape to new model calls.
    fn validation_schema(&self) -> Value {
        self.definition().input_schema
    }
    fn validate_arguments(&self, arguments: &Value) -> CoreResult<()> {
        let mut definition = self.definition();
        definition.input_schema = self.validation_schema();
        crate::tool_validation::validate_arguments(&definition, arguments)
    }
    fn side_effectful(&self) -> bool;
    /// Whether this concrete call can mutate the local workspace. Most tools
    /// use their static classification; tools such as a shell may recognize a
    /// deliberately tiny set of provably inert invocations so unrelated
    /// read/wait-only subagents are not serialized behind snapshot epochs.
    fn side_effectful_for(&self, arguments: &Value) -> bool {
        let _ = arguments;
        self.side_effectful()
    }
    /// Resolve live resource identities before permission evaluation. Implementations must
    /// only inspect metadata here, and bind execution to the captured identity afterwards.
    async fn prepare_arguments(
        &self,
        arguments: Value,
        _thread: ThreadId,
        _cancel: CancellationToken,
    ) -> CoreResult<Value> {
        Ok(arguments)
    }
    /// A live, tool-owned policy may tighten normal permissions but never loosen them.
    /// Ask constraints must still prompt when hooks or session rules pre-approve a tool.
    fn permission_constraint(
        &self,
        _arguments: &Value,
        _operation: &str,
    ) -> Option<crate::PermissionDecision> {
        None
    }
    /// An operation only a person may approve, such as a site in the user's
    /// own Chrome. Configured rules may deny it but never allow it; only an
    /// answer given in this chat satisfies it, and Auto mode, `--yolo` and
    /// hooks never answer it.
    fn explicit_approval(&self, _arguments: &Value, _operation: &str) -> bool {
        false
    }
    async fn execute(&self, arguments: Value, context: ToolContext) -> CoreResult<ToolOutput>;
}

pub trait ToolRegistry: Send + Sync {
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>>;
    fn list(&self) -> Vec<ToolDefinition>;
    /// The connected MCP servers that declare resources, for the resource
    /// tools' reach (see [`McpRegistry::resource_servers`]).
    fn resource_servers(&self) -> Vec<String> {
        Vec::new()
    }

    /// Exact registrations win. Repair only unambiguous capitalization or the
    /// known function namespace; never guess spellings or choose by permission.
    fn resolve_name(&self, name: &str) -> String {
        if self.get(name).is_some() {
            return name.to_owned();
        }
        crate::tool_validation::resolve_name(name, self.list().into_iter().map(|tool| tool.name))
    }
}

/// Workspace file snapshots backing undo/redo. Implementations own their
/// storage; the runtime only captures and restores.
#[async_trait]
pub trait SnapshotStore: Send + Sync {
    async fn capture(&self, thread: ThreadId) -> CoreResult<SnapshotInfo>;
    /// Legacy full-tree restore, retained for persisted pre-delta snapshots.
    /// Implementations must never follow workspace symlinks outside the root.
    async fn restore(&self, snapshot_id: &str) -> CoreResult<Vec<String>>;
    /// Restore only paths whose state differs between `expected_snapshot_id`
    /// and `desired_snapshot_id`. Every live path must still match `expected`;
    /// otherwise fail before writing so human/later edits are not clobbered.
    async fn restore_delta(
        &self,
        expected_snapshot_id: &str,
        desired_snapshot_id: &str,
    ) -> CoreResult<Vec<String>>;
    /// Compare one checkpoint with the live workspace for Last turn `/diff`.
    async fn diff_live(&self, snapshot_id: &str) -> CoreResult<Vec<haarness_protocol::FileDiff>>;
    /// Stable diff between a pre-mutation and completion checkpoint.
    async fn diff_snapshots(
        &self,
        before_snapshot_id: &str,
        after_snapshot_id: &str,
    ) -> CoreResult<Vec<haarness_protocol::FileDiff>>;
    /// Whether two checkpoints record any different file. The default keeps
    /// external snapshot adapters compatible.
    async fn snapshots_differ(
        &self,
        before_snapshot_id: &str,
        after_snapshot_id: &str,
    ) -> CoreResult<bool> {
        Ok(!self
            .diff_snapshots(before_snapshot_id, after_snapshot_id)
            .await?
            .is_empty())
    }
    /// Review metadata or one file. The default keeps external snapshot adapters compatible.
    async fn diff_review(
        &self,
        before: &str,
        after: Option<&str>,
        summary: bool,
        path: Option<&str>,
    ) -> CoreResult<Vec<haarness_protocol::FileDiff>> {
        let files = match after {
            Some(after) => self.diff_snapshots(before, after).await?,
            None => self.diff_live(before).await?,
        };
        Ok(files
            .into_iter()
            .filter(|file| path.is_none_or(|path| file.file == path))
            .map(|mut file| {
                if summary {
                    file.patch = None;
                    file.before = None;
                    file.after = None;
                }
                file
            })
            .collect())
    }
}

#[derive(Debug, Clone)]
pub struct SnapshotInfo {
    pub id: String,
    pub file_count: u64,
}

/// Durable secret storage. Implemented by the CLI over owner-only auth.json;
/// used by provider adapters to persist refreshed OAuth credentials.
#[async_trait]
pub trait CredentialStore: Send + Sync {
    async fn get(&self, namespace: &str, key: &str) -> CoreResult<Option<String>>;
    async fn set(&self, namespace: &str, key: &str, value: &str) -> CoreResult<()>;
    async fn remove(&self, namespace: &str, key: &str) -> CoreResult<()>;
}

/// The outcome of a lifecycle hook run: hooks can block an action, force a
/// permission decision, and inject extra context.
#[derive(Debug, Default, Clone)]
pub struct HookDecision {
    /// Block the action with this reason.
    pub block: Option<String>,
    /// Extra context appended to the tool result / prompt.
    pub context: Option<String>,
    /// Force the permission outcome (bypasses or forces the prompt).
    pub permission: Option<crate::PermissionDecision>,
    /// Replacement tool input returned by a PreToolUse hook.
    pub updated_input: Option<Value>,
    /// A `PermissionRequest` allow that answers Always: the runtime
    /// remembers what Always in the prompt remembers, where the request
    /// offered it to hooks.
    pub always: bool,
}

/// Claude-Code-compatible lifecycle hooks. Implemented by the composition
/// root from configuration; the core only invokes them.
#[async_trait]
pub trait HookPort: Send + Sync {
    /// Trusted adapters may append system context and adjust provider options.
    /// Durable transcript and runtime tool/permission selection stay unchanged.
    async fn prepare_request(
        &self,
        _thread: ThreadId,
        _request: &mut ProviderRequest,
    ) -> CoreResult<()> {
        Ok(())
    }
    async fn pre_tool_use(&self, thread: ThreadId, tool: &str, arguments: &Value) -> HookDecision;
    async fn post_tool_use(
        &self,
        thread: ThreadId,
        tool: &str,
        arguments: &Value,
        output: &str,
        is_error: bool,
    ) -> Option<String>;
    async fn user_prompt_submit(&self, thread: ThreadId, prompt: &str) -> HookDecision;
    /// A prompt is about to be shown for `permission` and the `patterns` it
    /// asks about. `detail` is the tool's arguments; when a pattern names
    /// where a file path really leads (a link's target), the runtime asks
    /// about it alone, with the arguments' path replaced by it. `always` is
    /// what an Always answer would remember, given only when the hook may
    /// answer Always for this request (see [`HookDecision::always`]).
    async fn permission_request(
        &self,
        thread: ThreadId,
        permission: &str,
        patterns: &[String],
        always: Option<&[String]>,
        detail: &Value,
    ) -> HookDecision;
    async fn stop(&self, thread: ThreadId);
    /// Observes only a committed compaction, never a discarded summary.
    async fn post_compact(&self, _thread: ThreadId, _trigger: &str, _item: &Item) {}
    /// Observes a terminal failure once, after automatic recovery is exhausted.
    async fn stop_failure(&self, _thread: ThreadId, _turn: TurnId, _detail: &Value) {}
    /// A thread was created (`SessionStart` / `session.start`).
    async fn session_start(&self, _thread: ThreadId, _agent: &str) {}
    /// A thread was archived (`SessionEnd` / `session.end`).
    async fn session_end(&self, _thread: ThreadId, _reason: &str) {}
    /// A root turn completed, after `stop` (`TurnEnd` / `chat.turn.end`).
    /// `detail` carries `turnId`, `agent`, `provider`, `model` and `finishReason`.
    async fn turn_end(&self, _thread: ThreadId, _turn: TurnId, _detail: &Value) {}
    /// Attention-worthy moments: a pending permission prompt or a completed
    /// turn (`Notification`). Observational only; never awaited on the hot path.
    async fn notification(&self, _thread: ThreadId, _kind: &str, _message: &str) {}
    /// A new configuration generation became active (`ConfigChange`).
    async fn config_change(&self, _detail: &Value) {}
    /// A slash command is about to expand (`command.execute.before`). A block
    /// aborts the command before its template runs; context joins the turn.
    async fn command_execute_before(
        &self,
        _thread: ThreadId,
        _command: &str,
        _arguments: &str,
    ) -> HookDecision {
        HookDecision::default()
    }
    /// Extra environment variables for one shell tool invocation (`shell.env`).
    async fn shell_env(&self, _thread: ThreadId, _cwd: &Path) -> BTreeMap<String, String> {
        BTreeMap::new()
    }
}

#[async_trait]
pub trait McpRegistry: Send + Sync {
    fn tool_snapshot(&self) -> Vec<ToolDefinition>;
    async fn list(&self) -> CoreResult<Vec<McpServerInfo>>;
    async fn configure(&self, name: &str, config: McpConfigureRequest)
    -> CoreResult<McpServerInfo>;
    async fn reconnect(&self, name: &str) -> CoreResult<McpServerInfo>;
    async fn disconnect(&self, name: &str) -> CoreResult<McpServerInfo>;
    async fn tools(&self, name: &str) -> CoreResult<Vec<ToolDefinition>>;
    async fn call(&self, name: &str, tool: &str, arguments: Value) -> CoreResult<ToolOutput>;
    /// Connected servers that declare the resources capability, by name.
    /// The model's resource tools are offered only while there is one.
    fn resource_servers(&self) -> Vec<String> {
        Vec::new()
    }
    async fn resources(&self, name: &str) -> CoreResult<Vec<Value>>;
    async fn resource_templates(&self, name: &str) -> CoreResult<Vec<Value>>;
    async fn read_resource(&self, name: &str, uri: &str) -> CoreResult<Value>;
    async fn prompts(&self, name: &str) -> CoreResult<Vec<Value>>;
    async fn get_prompt(&self, name: &str, prompt: &str, arguments: Value) -> CoreResult<Value>;
}

#[async_trait]
pub trait SkillRegistry: Send + Sync {
    async fn discover(&self) -> CoreResult<Vec<SkillInfo>>;
    async fn load(&self, name: &str) -> CoreResult<String>;
    async fn describe(&self, name: &str) -> CoreResult<SkillInfo> {
        self.discover()
            .await?
            .into_iter()
            .find(|s| s.name == name)
            .ok_or_else(|| crate::CoreError::NotFound(format!("skill {name}")))
    }
}

/// Explicit grants shared by linked worktrees of one local Git repository.
/// Implementations persist atomically and reload on each tool boundary.
pub trait RepositoryApprovalStore: Send + Sync {
    fn identity(&self) -> &str;
    fn load(&self) -> CoreResult<Vec<crate::PermissionRule>>;
    fn save(&self, rules: &[crate::PermissionRule]) -> CoreResult<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_dropped_calls_finish_incomplete_not_complete_or_failed() {
        for reason in [
            "MALFORMED_FUNCTION_CALL",
            "malformed_function_call",
            "UNEXPECTED_TOOL_CALL",
        ] {
            assert!(ProviderEvent::dropped_tool_call(reason), "{reason}");
            assert!(
                matches!(
                    ProviderEvent::finish_error(reason),
                    Some(CoreError::ProviderIncomplete(kept)) if kept == reason
                ),
                "{reason}"
            );
        }
        for reason in ["STOP", "tool_calls", "OTHER", "SAFETY", "error"] {
            assert!(!ProviderEvent::dropped_tool_call(reason), "{reason}");
        }
    }
}
