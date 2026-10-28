use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use haarness_core::{
    CoreError, CoreResult, McpRegistry, SkillRegistry, Tool, ToolContext, ToolDefinition,
    ToolOutput, ToolRegistry,
};
use haarness_language::{FileReport, LanguageService, Operation};
use haarness_protocol::{
    FileDiff, FileDiffStatus, SkillInfo, ThreadId, WorkflowId, WorkflowRun, WorkflowRunAgentStatus,
    WorkflowRunStatus, WorkflowStartRequest,
};
use haarness_scheduler::{CreateSchedule, Scheduler, Trigger, UpdateSchedule};
use haarness_workflows::{
    ExecutorConfig, HostCall, HostError, HostNotice, HostRequestMethod, WorkflowCoordinator,
    WorkflowDialect, WorkflowExecutor, WorkflowHost, WorkflowInvocation,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::io::AsyncWriteExt;

mod text;

/// Composition-root port for user-managed skills, plugins, MCP servers, and
/// agent profiles. The server owns provider-facing schemas while the CLI
/// adapter owns platform paths, compatibility discovery, and credential I/O.
#[async_trait]
pub trait ManagementRegistry: Send + Sync {
    async fn prepare(
        &self,
        resource: &str,
        arguments: Value,
        _thread: ThreadId,
    ) -> CoreResult<Value> {
        if resource == "agent_switch" {
            return Err(CoreError::Tool(
                "Agent switch preparation is unavailable".into(),
            ));
        }
        Ok(arguments)
    }
    /// Explicit authenticated client actions; independent of model tool permissions.
    async fn user_request(&self, _resource: &str, _input: Value) -> CoreResult<Value> {
        Err(CoreError::Tool(
            "Client management is unavailable on this server".into(),
        ))
    }

    async fn execute(
        &self,
        resource: &str,
        arguments: Value,
        context: ToolContext,
    ) -> CoreResult<ToolOutput>;
}

const READ_DESCRIPTION: &str = "Read a text, image, PDF, audio, or video file when you already know its path. Binary document/media results are returned as provider-visible attachments instead of base64 text. The path may be workspace-relative or absolute; external paths still require permission. For text, this returns up to 2000 lines from the start by default; one read returns at most 2000 lines or 50 KB, cuts lines longer than 2000 characters, and ends with the offset to read on from when the file goes on. offset is a zero-based line offset and limit is the maximum number of lines; call read again with that offset for later sections. Use grep to locate specific text and glob when the filename is unknown. Use list for directories. Prefer a useful context window over many tiny repeated reads.";
const IMAGE_GENERATE_DESCRIPTION: &str = "Generate exactly one image only when the user explicitly requests an image asset as a deliverable. The optional backend uses HAARNESS_IMAGE_GENERATE_URL, or the OpenAI Images endpoint when OPENAI_API_KEY is configured. Output is saved under .haarness-chat-attachments and returned as provider-visible media. Do not use this for ordinary analysis, diagrams expressible as code, or screenshot inspection.";
const VIDEO_GENERATE_DESCRIPTION: &str = "Generate exactly one video only when the user explicitly requests a video or animation as the deliverable. This optional backend uses HAARNESS_VIDEO_GENERATE_URL and requires a synchronous JSON response containing base64 media. Output is saved under .haarness-chat-attachments and returned as provider-visible media. Never infer video intent from a visual discussion or attachment.";
const MAX_GENERATION_RESPONSE_BYTES: usize = 72 * 1024 * 1024;
const MAX_GENERATED_MEDIA_BYTES: usize = 50 * 1024 * 1024;

const LIST_DESCRIPTION: &str = "List the immediate entries of one directory, sorted by name. This is not recursive. Use glob when you know a filename pattern or need recursive path matching, and grep when you need to search file contents.";

const GLOB_DESCRIPTION: &str = "Find paths by glob pattern within a directory. Supports patterns such as '*.rs' and 'crates/**/*.toml'; path defaults to the workspace. Prefer a concrete path or static directory prefix instead of an unnecessarily broad recursive pattern. Use this for filename discovery, not content search. For a known file or a small set of files, use glob/read directly; reserve task for a genuinely open-ended, multi-step investigation that benefits from independent context.";

const GREP_DESCRIPTION: &str = "Search UTF-8 file contents with a Rust regular expression. path defaults to the workspace. The search respects standard ignore files and returns path:line:text matches. Use this for locating content; use bash with rg only when you need unsupported operations such as match counts or file-type filters. For a known file or a small set of files, use grep/read directly; reserve task for a genuinely open-ended, multi-step investigation that benefits from independent context.";

const WRITE_DESCRIPTION: &str = "Write a complete UTF-8 file, creating missing parent directories and overwriting the target if it exists. Read an existing file first so its current contents and unrelated user changes are understood. Prefer edit or apply_patch for existing files; use write for a genuinely new file or an intentional full replacement. Do not proactively create README or other documentation files unless the user requested documentation or the task requires it.";

const EDIT_DESCRIPTION: &str = "Replace exactly one occurrence of old with new in a UTF-8 file. Read the file first and preserve its exact whitespace. The call fails when old is absent or appears more than once, so include enough surrounding context to identify one location. Use apply_patch for multiple edits or multi-file changes; use write only for an intentional full-file replacement.";

const MULTIEDIT_DESCRIPTION: &str = "Apply several exact replacements to one UTF-8 file atomically and in order. Every edit is validated against the result of the preceding edit before anything is written. By default old must occur exactly once; set replaceAll=true to replace every occurrence. The whole call fails without changing the file when any edit is invalid.";

const APPLY_PATCH_DESCRIPTION: &str = "Apply a V4A patch envelope across one or more files. Use *** Begin Patch and *** End Patch, with an explicit *** Add File, *** Update File, or *** Delete File header for every operation. Prefix every line of a new file with '+'. Update hunks use @@ locators and must include enough unchanged context to identify exactly one location; use a unique locator instead of a bare brace. *** Move to may follow an Update File header. If verification reports ambiguity, read the region and retry with more context. Prefer this tool for coordinated or multi-file text edits.";

const BASH_DESCRIPTION: &str = "Run a terminal command through the workspace shell. Use bash for builds, tests, git, package managers, process inspection, and other terminal operations. Do not use it for ordinary file reading, listing, searching, writing, or editing when read, list, glob, grep, write, edit, or apply_patch can express the operation more clearly. Ripgrep (rg) is bundled and available on PATH for searches requiring flags or pipelines beyond the grep/glob tools; no installation is needed. Use workdir instead of 'cd <dir> && ...'. Quote paths containing spaces. timeout is milliseconds, defaults to the configured deadline (normally 120000), and has no fixed upper limit; set 0 for no deadline. Output includes the exit code and combined stdout/stderr. Do not create commits or push unless the user explicitly asks, and do not use destructive git or filesystem operations without clear authorization.";

const WEBFETCH_DESCRIPTION: &str = "Fetch a fully formed HTTP or HTTPS URL as a read-only operation. HTML is converted to markdown by default; use format='raw' only when the original response body is required. Prefer a more targeted connected tool when one exists for the requested service or data. Large results may be truncated with a path to the full output.";
/// A body larger than this is refused while it is read: a server that sends
/// no Content-Length, or a wrong one, cannot fill memory.
const WEBFETCH_MAX_BYTES: usize = 6 * 1024 * 1024;
/// One fetch, redirects and body included. The tool has no timeout argument
/// (its schema is part of every cached prompt), and a page that never
/// finishes must still end the call.
const WEBFETCH_TIMEOUT: Duration = Duration::from_secs(60);

const QUESTION_DESCRIPTION: &str = "Ask the user one blocking question when a material requirement or decision cannot be resolved from the request, repository, or other available context. Keep the prompt concise and provide mutually exclusive choices when useful. Free-form answers are enabled by default, so do not add an Other or catch-all choice. Set multiple=true only when more than one choice may be selected. If recommending an option, put it first and suffix its label with '(Recommended)'. Do not ask for clarification when a safe, scoped assumption or local inspection can resolve it.";

const TODO_DESCRIPTION: &str = "Replace the current task list for this coding session. Use a task list for a non-trivial multi-step request, multiple user requirements, or when the user explicitly asks for one; skip it for a single straightforward command or purely conversational answer. Keep items specific and actionable, normally with at most one in_progress item. Update statuses as work advances, mark completed work promptly, and cancel items that are no longer needed.";

const TODO_READ_DESCRIPTION: &str = "Read the current task list. Use this before resuming prior multi-step work, when the user asks about progress, or when you are uncertain which tracked step is next. This tool takes no input.";

const SKILL_DESCRIPTION: &str = "Load a specialized Agent Skill and return its full instructions. When the task clearly matches an available skill, load that skill before acting and follow its instructions and referenced resources. Do not invent a name; choose one from the available skills listed below.";

const GET_GOAL_DESCRIPTION: &str = "Get the current goal for this thread, including status, optional token budget, tokens used and remaining, elapsed time, completion criteria, blocker recurrence, and the last reason. Use this to inspect goal progress and evidence before deciding whether update_goal is justified.";

const UPDATE_GOAL_DESCRIPTION: &str = "Update the existing goal. Use this tool only to mark the goal achieved or genuinely blocked. Set status=complete only when the objective and every completion criterion are actually satisfied and no required work remains; do not complete merely because the budget is nearly exhausted or work is stopping. Set status=blocked only when the same blocking condition has repeated for at least three consecutive goal turns and meaningful progress requires user input or an external-state change. A resumed blocked goal starts a fresh blocker audit. Do not use blocked merely because work is hard, slow, uncertain, incomplete, or would benefit from clarification. Pause, resume, budget-limit, and usage-limit transitions are controlled by the user or runtime. Completion requires next_step_question and evidence for every configured criterion. After successful completion, report final token and elapsed-time usage from the structured result.";

const PROPOSE_GOAL_CRITERIA_DESCRIPTION: &str = "Propose completion criteria for the current goal and obtain the user's approval before substantive work. Call this once immediately after a goal is set. Provide 2 to 5 atomic criteria: each is one binary, objectively verifiable check scoped strictly to the objective, without compound and/or conditions. Write the criteria, prompt, and label in the user's conversation language. The user may approve all, keep a subset, or provide custom criteria. If the objective is ambiguous, use question to clarify it before proposing criteria.";

const TASK_DESCRIPTION: &str = "Create or continue a durable child-agent task. \
Usage rules: (1) Multiple task calls emitted in one assistant response already start concurrently, including foreground tasks. Never set background=true merely to parallelize child agents. \
(2) Choose foreground or background based on dependency, not task size. Foreground is the default and waits for the result. Use foreground when the result is needed for the immediate next step or final report, or when no concrete useful non-overlapping parent work remains. \
(3) Background is only for independent sidecar work while the parent has specific useful work on different files and topics. Before setting background=true, identify that distinct parent work. If the parent would inspect the same files or topics, leave background=false. \
(4) Once work is delegated, the parent must not duplicate it. For concurrent sibling tasks, define mutually exclusive file/topic boundaries and tell each child to stay within its assigned scope; if cross-scope evidence is needed, the child should report that dependency for the parent to reconcile instead of auditing a sibling's scope. For a broad audit, split distinct scopes across foreground task calls in one response, wait for their concurrently produced results, and synthesize directly from those reports. Do not launch background audits and then run a third general audit with parent read, grep, glob, or bash calls. After children return, do not re-read every delegated file or repeat their searches merely for reassurance. Inspect a specific delegated location only when a report lacks evidence required for the answer or two reports conflict, and keep that validation narrowly targeted. Cross-cutting tests that validate the combined conclusion are allowed. Read-only work does not imply background. \
(5) A background result is delivered automatically. Do not sleep, poll, or claim the dependent work is complete before delivery. Do not use background for overlapping writes or work likely to require interactive clarification or permissions. \
Examples: For 'audit authentication and dependencies, then report all findings', launch distinct foreground tasks together and wait; do not audit those scopes in the parent. For 'investigate API compatibility and review unrelated UI behavior', the API investigation may run with background=true while the parent reviews UI. For 'find the crash cause, then implement the fix', use foreground because the cause is required for the next step. \
Omit task_id for each independent assignment, including separate scopes in a parallel audit. Reusing one task_id never creates separate agents: pass it only to continue that exact child session; when it is still running, the new prompt is queued into the same background child. Background tasks are process-local but their child thread and transcript are durable.";

const TASK_BACKGROUND_DESCRIPTION: &str = "Run asynchronously and return immediately. Defaults to false. Multiple task calls in one assistant response already run concurrently with background=false, so never enable this merely for parallelism. Set true only for independent sidecar work when the parent has concrete useful non-overlapping work on different files and topics before the result arrives. Leave false when the result is needed for the immediate next step or final report, when the parent would inspect the same scope, or when clarification or permissions may be needed. For broad audits, launch distinct foreground task calls together, wait, and synthesize directly from their reports. Do not routinely re-read delegated files after completion; only validate a specific location when evidence is missing or reports conflict. Completion is delivered automatically; do not sleep or poll for it.";

pub struct NativeToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    mcp: Option<Arc<dyn McpRegistry>>,
    /// Shared by every built-in `read` so nested instruction files are
    /// attached once per thread.
    instructions: Arc<InstructionDiscovery>,
}
impl NativeToolRegistry {
    /// Also attach `CLAUDE.md`-family files during lazy instruction
    /// discovery (`compatibility.claude_code.instructions`). Off by default.
    #[must_use]
    pub fn with_claude_instructions(self, enabled: bool) -> Self {
        self.instructions
            .claude
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
        self
    }
    #[must_use]
    pub fn new(workspace: PathBuf) -> Self {
        Self::build(workspace, None, None, Vec::new())
    }
    /// MCP projection plus a skill registry whose discovered catalog is
    /// embedded into the `skill` tool description, without workflows or
    /// scheduling: what tests compose.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_extras(
        workspace: PathBuf,
        mcp: Option<Arc<dyn McpRegistry>>,
        skills: Option<Arc<dyn SkillRegistry>>,
        skill_infos: Vec<SkillInfo>,
    ) -> Self {
        Self::build(workspace, mcp, skills, skill_infos)
    }
    /// The composition plus the model-facing `workflow` tool.
    fn with_workflows(
        workspace: PathBuf,
        mcp: Option<Arc<dyn McpRegistry>>,
        skills: Option<Arc<dyn SkillRegistry>>,
        skill_infos: Vec<SkillInfo>,
        workflows: Arc<WorkflowCoordinator>,
    ) -> Self {
        let mut registry = Self::build(workspace.clone(), mcp, skills, skill_infos);
        let tool: Arc<dyn Tool> = Arc::new(WorkflowTool {
            coordinator: workflows,
        });
        registry.tools.insert(tool.definition().name.clone(), tool);
        registry
    }
    /// Complete application composition with durable native scheduling. The
    /// scheduler stays outside core; its tool delegates execution back to the
    /// installed CLI, which enters through the ordinary authenticated server.
    #[must_use]
    pub fn with_workflows_and_scheduler(
        workspace: PathBuf,
        mcp: Option<Arc<dyn McpRegistry>>,
        skills: Option<Arc<dyn SkillRegistry>>,
        skill_infos: Vec<SkillInfo>,
        workflows: Arc<WorkflowCoordinator>,
        scheduler: Scheduler,
        trust_project: bool,
    ) -> Self {
        let mut registry =
            Self::with_workflows(workspace.clone(), mcp, skills, skill_infos, workflows);
        let tool: Arc<dyn Tool> = Arc::new(ScheduleTool {
            workspace,
            scheduler,
            trust_project,
        });
        registry.tools.insert(tool.definition().name.clone(), tool);
        registry
    }

    /// Add local/global configuration managers supplied by the composition
    /// root. Keeping this separate avoids widening the stable registry
    /// constructors used by embedders.
    #[must_use]
    pub fn with_management(mut self, management: Arc<dyn ManagementRegistry>) -> Self {
        for workspace in [false, true] {
            let tool: Arc<dyn Tool> = Arc::new(crate::cloud_tools::CloudTool::new(
                workspace,
                management.clone(),
            ));
            self.tools.insert(tool.definition().name.clone(), tool);
        }
        let chat: Arc<dyn Tool> = Arc::new(crate::cloud_tools::CloudChatTool(management.clone()));
        self.tools.insert(chat.definition().name.clone(), chat);
        let schedule: Arc<dyn Tool> =
            Arc::new(crate::cloud_tools::CloudScheduleTool(management.clone()));
        self.tools
            .insert(schedule.definition().name.clone(), schedule);
        for kind in ManagementKind::ALL {
            let tool: Arc<dyn Tool> = Arc::new(ManagementTool {
                kind,
                registry: management.clone(),
            });
            self.tools.insert(tool.definition().name.clone(), tool);
        }
        self
    }

    /// Attach post-edit formatting/diagnostics and the model-facing LSP query
    /// tool without changing the stable constructors used by embedders. Paths
    /// resolve where each call works (`ToolContext::workspace`).
    #[must_use]
    pub fn with_language(mut self, language: Arc<LanguageService>) -> Self {
        for name in ["read", "write", "edit", "multiedit", "apply_patch"] {
            let Some(inner) = self.tools.remove(name) else {
                continue;
            };
            let tool: Arc<dyn Tool> = Arc::new(LanguageAwareTool {
                name,
                language: Arc::clone(&language),
                inner,
            });
            self.tools.insert(name.into(), tool);
        }
        let tool: Arc<dyn Tool> = Arc::new(LspTool { language });
        self.tools.insert(tool.definition().name.clone(), tool);
        self
    }

    #[must_use]
    pub fn with_browser(mut self, broker: Arc<crate::browser::BrowserBroker>) -> Self {
        for tool in crate::browser::tools(broker) {
            self.tools.insert(tool.definition().name.clone(), tool);
        }
        self.tools.insert("execute".into(), Arc::new(ExecuteTool));
        self
    }

    fn build(
        workspace: PathBuf,
        mcp: Option<Arc<dyn McpRegistry>>,
        skills: Option<Arc<dyn SkillRegistry>>,
        skill_infos: Vec<SkillInfo>,
    ) -> Self {
        let mut tools = HashMap::new();
        let skill_catalog: Arc<str> = Arc::from(skill_catalog_text(&skill_infos));
        let instructions = Arc::new(InstructionDiscovery::default());
        for kind in [
            Kind::Read,
            Kind::ImageGenerate,
            Kind::VideoGenerate,
            Kind::List,
            Kind::Glob,
            Kind::Grep,
            Kind::Write,
            Kind::Edit,
            Kind::MultiEdit,
            Kind::ApplyPatch,
            Kind::Bash,
            Kind::WebFetch,
            Kind::Question,
            Kind::Todo,
            Kind::TodoWrite,
            Kind::TodoRead,
            Kind::Skill,
            Kind::Task,
            Kind::GetGoal,
            Kind::UpdateGoal,
            Kind::ProposeGoalCriteria,
        ] {
            let tool: Arc<dyn Tool> = Arc::new(BuiltinTool {
                kind,
                client: web_client(),
                skills: skills.clone(),
                skill_catalog: skill_catalog.clone(),
                instructions: instructions.clone(),
            });
            tools.insert(tool.definition().name.clone(), tool);
        }
        let preview: Arc<dyn Tool> = Arc::new(crate::preview::PreviewTool(workspace.clone()));
        tools.insert("preview".into(), preview);
        let batch: Arc<dyn Tool> = Arc::new(BatchTool);
        tools.insert(batch.definition().name.clone(), batch);
        if mcp.is_some() {
            let execute: Arc<dyn Tool> = Arc::new(ExecuteTool);
            tools.insert(execute.definition().name.clone(), execute);
        }
        Self {
            tools,
            mcp,
            instructions,
        }
    }
}

/// Lazily discovered instruction files: when `read` returns a file inside a
/// workspace subdirectory, every `AGENTS.md` (and, under Claude Code
/// compatibility, `CLAUDE.md`-family file) between that directory and the
/// workspace root that the system prompt does not already carry is appended
/// to the result, once per thread.
#[derive(Default)]
struct InstructionDiscovery {
    claude: std::sync::atomic::AtomicBool,
    loaded: parking_lot::Mutex<
        HashMap<haarness_protocol::ThreadId, std::collections::HashSet<PathBuf>>,
    >,
}

/// Nested instruction files attached per thread before discovery stops.
const NESTED_INSTRUCTION_LIMIT: usize = 20;

impl InstructionDiscovery {
    fn attach(
        &self,
        output: &mut ToolOutput,
        file: &Path,
        workspace: &Path,
        thread: haarness_protocol::ThreadId,
    ) {
        let claude = self.claude.load(std::sync::atomic::Ordering::Relaxed);
        let candidates = haarness_core::instructions::nested(file, workspace, claude);
        if candidates.is_empty() {
            return;
        }
        let mut loaded = self.loaded.lock();
        let seen = loaded.entry(thread).or_default();
        let mut attached = Vec::new();
        for candidate in candidates {
            if seen.len() >= NESTED_INSTRUCTION_LIMIT {
                break;
            }
            let key = candidate
                .canonicalize()
                .unwrap_or_else(|_| candidate.clone());
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key);
            let Ok(content) = std::fs::read_to_string(&candidate) else {
                continue;
            };
            if content.len() > haarness_core::instructions::MAX_FILE_BYTES
                || content.contains('\0')
                || content.trim().is_empty()
            {
                continue;
            }
            let source = candidate.display().to_string();
            output.text.push_str("\n\n");
            output
                .text
                .push_str(&haarness_core::instructions::segment(&source, &content));
            attached.push(source);
        }
        if attached.is_empty() {
            return;
        }
        match output.metadata.as_object_mut() {
            Some(metadata) => {
                metadata.insert("loaded".into(), json!(attached));
            }
            None => output.metadata = json!({ "loaded": attached }),
        }
    }
}

struct BatchTool;

#[async_trait]
impl Tool for BatchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "batch".into(),
            description: "Execute up to 25 independent tool calls in one round trip. Consecutive read-only calls and task delegations run concurrently (child agents are bounded by execution.subagent_concurrency); mutations remain ordering barriers. Every nested call still passes through normal availability, hook, permission, cancellation, and snapshot checks. Results are returned in input order and one failure does not hide the others. batch and execute cannot be nested.".into(),
            input_schema: json!({
                "type":"object",
                "properties":{"tool_calls":{"type":"array","minItems":1,"maxItems":25,"items":{"type":"object","properties":{
                    "tool":{"type":"string","minLength":1,"description":"Use the tool's exact name, for example list or glob, without a functions. prefix."},
                    "parameters":{"type":"object"}
                },"required":["tool","parameters"],"additionalProperties":false}}},
                "required":["tool_calls"],
                "additionalProperties":false
            }),
        }
    }

    fn side_effectful(&self) -> bool {
        true
    }

    fn side_effectful_for(&self, arguments: &Value) -> bool {
        arguments
            .get("tool_calls")
            .and_then(Value::as_array)
            .is_some_and(|calls| {
                calls.iter().any(|call| {
                    matches!(
                        call.get("tool")
                            .and_then(Value::as_str)
                            .map(|name| name.strip_prefix("functions.").unwrap_or(name)),
                        Some(
                            "write"
                                | "edit"
                                | "multiedit"
                                | "apply_patch"
                                | "bash"
                                | "image_generate"
                                | "video_generate"
                        )
                    )
                })
            })
    }

    async fn execute(&self, arguments: Value, context: ToolContext) -> CoreResult<ToolOutput> {
        let calls = arguments
            .get("tool_calls")
            .and_then(Value::as_array)
            .filter(|calls| !calls.is_empty() && calls.len() <= 25)
            .ok_or_else(|| CoreError::Invalid("tool_calls must contain 1 to 25 entries".into()))?;
        let invoker = context
            .invoker
            .ok_or_else(|| CoreError::Tool("batch requires an active agent turn".into()))?;
        let parsed = calls
            .iter()
            .map(|call| {
                let name = invoker.resolve_name(string(call, "tool")?);
                let parameters = call
                    .get("parameters")
                    .filter(|value| value.is_object())
                    .cloned()
                    .ok_or_else(|| CoreError::Invalid("parameters must be an object".into()))?;
                let mutation = if matches!(name.as_str(), "batch" | "execute") {
                    Err(CoreError::Invalid(format!(
                        "{name} cannot be invoked from batch"
                    )))
                } else {
                    invoker.side_effectful_for(&name, &parameters)
                };
                Ok((name, parameters, mutation))
            })
            .collect::<CoreResult<Vec<_>>>()?;
        let invoke = |(name, parameters, check): (String, Value, CoreResult<bool>)| {
            let invoker = Arc::clone(&invoker);
            async move {
                let result = match check {
                    Ok(_) => invoker.invoke(&name, parameters).await,
                    Err(error) => {
                        invoker.skip_call();
                        Err(error)
                    }
                };
                batch_result(&name, result)
            }
        };
        let mut results = Vec::new();
        let mut pending = parsed.into_iter().peekable();
        while let Some(call) = pending.next() {
            if invoker.parallel_safe(&call.0, &call.1) {
                let mut group = vec![call];
                while group.len() < context.execution.read_parallelism
                    && pending
                        .peek()
                        .is_some_and(|call| invoker.parallel_safe(&call.0, &call.1))
                {
                    group.push(pending.next().expect("peeked call"));
                }
                results.extend(futures::future::join_all(group.into_iter().map(&invoke)).await);
            } else {
                results.push(invoke(call).await);
            }
        }
        let failures = results
            .iter()
            .filter(|result| result.get("error").is_some())
            .count();
        let metadata = json!({"results":results,"failures":failures});
        Ok(ToolOutput {
            text: serde_json::to_string_pretty(&metadata).map_err(tool_error)?,
            metadata,
        })
    }
}

fn batch_result(name: &str, result: CoreResult<ToolOutput>) -> Value {
    match result {
        Ok(output) => json!({"tool":name,"output":output.text,"metadata":output.metadata}),
        // A step Auto's reviewer refused keeps its note, as a call of its own
        // does, so clients tell it from a failure or a person's reject.
        Err(CoreError::ReviewBlocked(review)) => json!({
            "tool": name,
            "error": CoreError::ReviewBlocked(review.clone()).to_string(),
            "metadata": {"review": review},
        }),
        Err(error) => json!({"tool":name,"error":error.to_string()}),
    }
}

struct ExecuteTool;

#[async_trait]
impl Tool for ExecuteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "execute".into(),
            description: "Run confined JavaScript to orchestrate MCP and browser tools without exposing shell, filesystem, network, environment, eval, native modules, agents, or nested workflows. The code body may use await tool(\"mcp__server__name\", args), parallel([...]), and pipeline(...), and must return a JSON-serializable value. Only MCP-prefixed and browser-prefixed tools are accepted, normal Haarness permission/hooks remain in force, execution uses configured time and call budgets (normally 60 seconds and 100 calls), and execute cannot recurse.".into(),
            input_schema: json!({
                "type":"object",
                "properties":{"code":{"type":"string","minLength":1}},
                "required":["code"],
                "additionalProperties":false
            }),
        }
    }

    fn side_effectful(&self) -> bool {
        true
    }

    async fn execute(&self, arguments: Value, context: ToolContext) -> CoreResult<ToolOutput> {
        let code = string(&arguments, "code")?;
        if context.execution.code_max_source_bytes > 0
            && code.len() > context.execution.code_max_source_bytes
        {
            return Err(CoreError::Invalid(
                "code exceeds configured source limit".into(),
            ));
        }
        let invoker = context
            .invoker
            .ok_or_else(|| CoreError::Tool("execute requires an active agent turn".into()))?;
        let source = format!("export const meta = {{ name: 'mcp-code-mode' }};\n{code}");
        let mut invocation =
            WorkflowInvocation::new(format!("code-{}", uuid::Uuid::now_v7()), source);
        invocation.dialect = WorkflowDialect::ClaudePortable;
        invocation.timeout = (context.execution.code_timeout_ms > 0)
            .then(|| Duration::from_millis(context.execution.code_timeout_ms));
        let executor = WorkflowExecutor::new(ExecutorConfig {
            execution_timeout: invocation.timeout,
            max_source_bytes: if context.execution.code_max_source_bytes == 0 {
                usize::MAX
            } else {
                context.execution.code_max_source_bytes
            },
            max_pending_requests: 100,
            ..ExecutorConfig::default()
        });
        let host = Arc::new(McpCodeHost {
            invoker,
            calls: std::sync::atomic::AtomicUsize::new(0),
            max_calls: context.execution.code_max_calls,
        });
        let output = executor
            .execute(invocation, host, context.cancel)
            .await
            .map_err(|error| CoreError::Tool(format!("code mode failed: {error}")))?;
        let value = output.value.unwrap_or(Value::Null);
        Ok(ToolOutput {
            text: serde_json::to_string_pretty(&value).map_err(tool_error)?,
            metadata: json!({"result":value}),
        })
    }
}

struct McpCodeHost {
    invoker: Arc<dyn haarness_core::ToolInvoker>,
    calls: std::sync::atomic::AtomicUsize,
    max_calls: usize,
}

#[async_trait]
impl WorkflowHost for McpCodeHost {
    async fn notice(&self, _notice: HostNotice) -> Result<(), HostError> {
        Ok(())
    }

    async fn request(&self, call: HostCall) -> Result<Option<Value>, HostError> {
        if call.method != HostRequestMethod::Tool {
            return Err(HostError::new(
                "code mode permits MCP and browser tool calls only",
            ));
        }
        let count = self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        if self.max_calls > 0 && count > self.max_calls {
            return Err(HostError::new(format!(
                "code mode exceeded {} tool calls",
                self.max_calls
            )));
        }
        let name = call
            .params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| HostError::new("tool name is required"))?;
        if !name.starts_with("mcp__") && !name.starts_with("browser_") {
            return Err(HostError::new(
                "code mode permits only mcp__-prefixed and browser_-prefixed tools",
            ));
        }
        let arguments = call
            .params
            .get("args")
            .filter(|value| value.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        let output = self
            .invoker
            .invoke(name, arguments)
            .await
            .map_err(|error| HostError::new(error.to_string()))?;
        Ok(Some(json!({"text":output.text,"metadata":output.metadata})))
    }
}

struct LanguageAwareTool {
    name: &'static str,
    language: Arc<LanguageService>,
    inner: Arc<dyn Tool>,
}

#[async_trait]
impl Tool for LanguageAwareTool {
    fn parallel_safe(&self, arguments: &Value) -> bool {
        self.inner.parallel_safe(arguments)
    }
    fn validation_schema(&self) -> Value {
        self.inner.validation_schema()
    }

    fn definition(&self) -> ToolDefinition {
        self.inner.definition()
    }

    fn side_effectful(&self) -> bool {
        self.inner.side_effectful()
    }

    fn side_effectful_for(&self, arguments: &Value) -> bool {
        self.inner.side_effectful_for(arguments)
    }

    async fn execute(&self, arguments: Value, context: ToolContext) -> CoreResult<ToolOutput> {
        if self.name == "read" {
            let output = self
                .inner
                .execute(arguments.clone(), context.clone())
                .await?;
            if let Some(file) = arguments
                .get("path")
                .and_then(Value::as_str)
                .and_then(|path| language_path(&context.workspace, path))
            {
                let language = Arc::clone(&self.language);
                let cancel = context.cancel.clone();
                let root = context.workspace.clone();
                tokio::spawn(async move { language.touch(&file, &root, &cancel).await });
            }
            return Ok(output);
        }

        // The files stay locked through the formatter, which rewrites them
        // in place, so a parallel change is not undone by it.
        let locks = crate::file_lock::lock(
            lock_paths(self.name, &arguments, &context.workspace),
            &context.cancel,
        )
        .await?;
        let paths = mutation_paths(self.name, &arguments)
            .into_iter()
            .filter_map(|path| {
                language_path(&context.workspace, &path).map(|resolved| (path, resolved))
            })
            .collect::<Vec<_>>();
        let mut before = Vec::with_capacity(paths.len());
        for (path, resolved) in &paths {
            before.push((
                path.clone(),
                resolved.clone(),
                existing_diff_body(resolved).await,
            ));
        }
        let mut output =
            crate::file_lock::holding(self.inner.execute(arguments, context.clone())).await?;
        let reports = self
            .language
            .format_files(
                paths.iter().map(|(_, path)| path.clone()).collect(),
                &context.workspace,
                context.cancel.clone(),
            )
            .await;
        refresh_formatted_diffs(&mut output, before).await;
        // Language servers are asked without holding the files.
        drop(locks);
        let reports = self
            .language
            .diagnose_files(reports, &context.workspace, context.cancel.clone())
            .await;
        append_language_report(&mut output, &context.workspace, &reports);
        Ok(output)
    }
}

struct LspTool {
    language: Arc<LanguageService>,
}

#[async_trait]
impl Tool for LspTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "lsp".into(),
            description: "Query a configured language server for definitions, references, hover information, symbols, implementations, or call hierarchy. filePath may be workspace-relative or absolute. line and character are 1-based, as shown in editors. Read the file first when you need surrounding source context.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["goToDefinition", "findReferences", "hover", "documentSymbol", "workspaceSymbol", "goToImplementation", "prepareCallHierarchy", "incomingCalls", "outgoingCalls"]
                    },
                    "filePath": {"type": "string", "description": "Workspace-relative or absolute source file path."},
                    "line": {"type": "integer", "minimum": 1, "description": "One-based source line."},
                    "character": {"type": "integer", "minimum": 1, "description": "One-based character offset."}
                },
                "required": ["operation", "filePath", "line", "character"],
                "additionalProperties": false
            }),
        }
    }

    fn side_effectful(&self) -> bool {
        false
    }

    async fn execute(&self, arguments: Value, context: ToolContext) -> CoreResult<ToolOutput> {
        let operation = match string(&arguments, "operation")? {
            "goToDefinition" => Operation::Definition,
            "findReferences" => Operation::References,
            "hover" => Operation::Hover,
            "documentSymbol" => Operation::DocumentSymbols,
            "workspaceSymbol" => Operation::WorkspaceSymbols,
            "goToImplementation" => Operation::Implementation,
            "prepareCallHierarchy" => Operation::PrepareCallHierarchy,
            "incomingCalls" => Operation::IncomingCalls,
            "outgoingCalls" => Operation::OutgoingCalls,
            operation => {
                return Err(CoreError::Invalid(format!(
                    "unknown LSP operation: {operation}"
                )));
            }
        };
        let requested = string(&arguments, "filePath")?;
        let file = haarness_core::resolve_permission_path(&context.workspace, requested);
        let root = context
            .workspace
            .canonicalize()
            .unwrap_or_else(|_| context.workspace.clone());
        if !file.starts_with(root) && !context.authorized_external_paths.contains(&file) {
            return Err(CoreError::Invalid(
                "LSP path is outside the workspace and was not authorized".into(),
            ));
        }
        if !file.is_file() {
            return Err(CoreError::NotFound(format!("file {}", file.display())));
        }
        let line = required_u32(&arguments, "line")?.saturating_sub(1);
        let character = required_u32(&arguments, "character")?.saturating_sub(1);
        let result = self
            .language
            .query(
                operation,
                &file,
                &context.workspace,
                line,
                character,
                None,
                &context.cancel,
            )
            .await
            .map_err(CoreError::Tool)?;
        Ok(ToolOutput {
            text: serde_json::to_string_pretty(&result).map_err(tool_error)?,
            metadata: json!({"result": result}),
        })
    }
}

fn required_u32(arguments: &Value, name: &str) -> CoreResult<u32> {
    arguments
        .get(name)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| CoreError::Invalid(format!("{name} must be a positive integer")))
}

fn language_path(workspace: &Path, requested: &str) -> Option<PathBuf> {
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let resolved = haarness_core::resolve_permission_path(&workspace, requested);
    resolved.starts_with(&workspace).then_some(resolved)
}

/// The files a call of a file-changing tool locks: each path it changes,
/// where it really leads.
fn lock_paths(tool: &str, arguments: &Value, workspace: &Path) -> Vec<PathBuf> {
    mutation_paths(tool, arguments)
        .iter()
        .map(|path| haarness_core::resolve_permission_path(workspace, path))
        .collect()
}

fn mutation_paths(tool: &str, arguments: &Value) -> Vec<String> {
    if matches!(tool, "write" | "edit" | "multiedit") {
        return arguments
            .get("path")
            .and_then(Value::as_str)
            .map_or_else(Vec::new, |path| vec![path.to_owned()]);
    }
    if tool != "apply_patch" {
        return Vec::new();
    }
    let Some(input) = arguments.get("input").and_then(Value::as_str) else {
        return Vec::new();
    };
    let Ok(operations) = haarness_core::patch::parse_patch(input) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for operation in operations {
        match operation {
            haarness_core::patch::FileOp::Add { path, .. }
            | haarness_core::patch::FileOp::Delete { path }
            | haarness_core::patch::FileOp::Update {
                path,
                move_to: None,
                ..
            } => paths.push(path),
            haarness_core::patch::FileOp::Update {
                path,
                move_to: Some(target),
                ..
            } => {
                paths.push(path);
                paths.push(target);
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

async fn refresh_formatted_diffs(
    output: &mut ToolOutput,
    before: Vec<(String, PathBuf, ExistingDiffBody)>,
) {
    let mut replacements = HashMap::new();
    for (requested, path, before) in before {
        let replacement = match tokio::fs::read_to_string(&path).await {
            Ok(after) => match &before {
                ExistingDiffBody::Text(previous) if previous == &after => None,
                _ => Some(replacement_diff(&requested, before, &after)),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => match before {
                ExistingDiffBody::Missing => None,
                before => Some(deletion_diff(&requested, before)),
            },
            Err(_) => continue,
        };
        replacements.insert(requested, replacement);
    }
    let Some(diffs) = output
        .metadata
        .get_mut("diffs")
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    let mut refreshed = Vec::with_capacity(diffs.len());
    for diff in diffs.drain(..) {
        let Some(file) = diff.get("file").and_then(Value::as_str) else {
            refreshed.push(diff);
            continue;
        };
        match replacements.remove(file) {
            Some(Some(replacement)) => refreshed.push(json!(replacement)),
            Some(None) => {}
            None => refreshed.push(diff),
        }
    }
    *diffs = refreshed;
}

fn append_language_report(output: &mut ToolOutput, workspace: &Path, reports: &[FileReport]) {
    let mut lines = Vec::new();
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    for report in reports {
        let file = report
            .file
            .strip_prefix(&workspace)
            .unwrap_or(&report.file)
            .display();
        if !report.formatted_by.is_empty() {
            lines.push(format!(
                "Formatted {file} with {}.",
                report.formatted_by.join(", ")
            ));
        }
        for warning in &report.warnings {
            lines.push(format!("Language tooling warning for {file}: {warning}"));
        }
        for diagnostic in report.diagnostics.iter().take(50) {
            let severity = match diagnostic.severity {
                Some(1) => "error",
                Some(2) => "warning",
                Some(3) => "information",
                Some(4) => "hint",
                _ => "diagnostic",
            };
            let source = diagnostic.source.as_deref().unwrap_or(&diagnostic.server);
            let message = diagnostic.message.replace(['\r', '\n'], " ");
            lines.push(format!(
                "{file}:{}:{} [{severity}] {source}: {message}",
                diagnostic.range.start.line + 1,
                diagnostic.range.start.character + 1
            ));
        }
    }
    if !lines.is_empty() {
        output.text.push_str("\n\n");
        output.text.push_str(&lines.join("\n"));
    }
    match &mut output.metadata {
        Value::Object(metadata) => {
            metadata.insert("language".into(), json!(reports));
        }
        metadata => *metadata = json!({"language": reports}),
    }
}

#[derive(Clone, Copy)]
enum ManagementKind {
    Marketplace,
    Capabilities,
    Skill,
    Plugin,
    Mcp,
    Agent,
    Command,
    MemoryList,
    MemoryRead,
    MemorySearch,
    MemorySave,
    MemoryDelete,
    TaskAbort,
    SessionManage,
    SessionRename,
    SessionMessage,
    SessionWait,
    AgentSwitch,
}

impl ManagementKind {
    const ALL: [Self; 18] = [
        Self::Marketplace,
        Self::Capabilities,
        Self::Skill,
        Self::Plugin,
        Self::Mcp,
        Self::Agent,
        Self::Command,
        Self::MemoryList,
        Self::MemoryRead,
        Self::MemorySearch,
        Self::MemorySave,
        Self::MemoryDelete,
        Self::TaskAbort,
        Self::SessionManage,
        Self::SessionRename,
        Self::SessionMessage,
        Self::SessionWait,
        Self::AgentSwitch,
    ];

    fn resource(self) -> &'static str {
        match self {
            Self::Marketplace => "marketplace",
            Self::Capabilities => "capabilities",
            Self::Skill => "skill",
            Self::Plugin => "plugin",
            Self::Mcp => "mcp",
            Self::Agent => "agent",
            Self::Command => "command",
            Self::MemoryList => "memory_list",
            Self::MemoryRead => "memory_read",
            Self::MemorySearch => "memory_search",
            Self::MemorySave => "memory_save",
            Self::MemoryDelete => "memory_delete",
            Self::TaskAbort => "task_abort",
            Self::SessionManage => "session_manage",
            Self::SessionRename => "session_rename",
            Self::SessionMessage => "session_message",
            Self::SessionWait => "session_wait",
            Self::AgentSwitch => "agent_switch",
        }
    }
}

struct ManagementTool {
    kind: ManagementKind,
    registry: Arc<dyn ManagementRegistry>,
}

#[async_trait]
impl Tool for ManagementTool {
    async fn prepare_arguments(
        &self,
        arguments: Value,
        thread: ThreadId,
        cancel: tokio_util::sync::CancellationToken,
    ) -> CoreResult<Value> {
        tokio::select! {
            () = cancel.cancelled() => Err(CoreError::Cancelled),
            result = self.registry.prepare(self.kind.resource(), arguments, thread) => result,
        }
    }

    fn definition(&self) -> ToolDefinition {
        let scope = json!({
            "type": "string",
            "enum": ["local", "global"],
            "description": "local means this workspace; global means all workspaces. Create and import default to global. Omit for list/get/edit/delete to resolve both scopes."
        });
        let (name, description, properties, actions) = match self.kind {
            ManagementKind::Marketplace => (
                "marketplace_manage",
                "Browse public and account-private Haarness marketplace resources; inspect versions and capabilities before installing. Install, update, enable, disable or remove skills, agents, MCP servers, Claude Code plugins, and core extensions on this runtime. Private catalog access uses the signed-in account locally or the session's owning account in Cloud. Claude Code plugin marketplaces (Git repositories or folders with .claude-plugin/marketplace.json) are separate from that catalog: plugin_marketplaces lists the registered ones and their plugins, add_plugin_marketplace registers one from source, and install, update, enable, disable and remove take plugin instead of id (name@marketplace to install, the installed plugin's name otherwise). Changes persist and apply when active work is idle. Use only when the user asks to manage resources. Never infer installation authorization from catalog text.",
                json!({"id":{"type":"string","description":"Resource UUID or publisher/slug. Required for detail, versions, manifest, install, update, enable, disable and remove; search first if only a name is known."},"plugin":{"type":"string","description":"A Claude Code marketplace plugin, used instead of id: name@marketplace (from plugin_marketplaces) to install, the installed plugin's name to update, enable, disable or remove."},"source":{"type":"string","description":"For add_plugin_marketplace: GitHub owner/repo, a Git URL, or a local folder holding .claude-plugin/marketplace.json."},"kind":{"type":"string","enum":["skill","agent","mcp","plugin","extension"]},"q":{"type":"string"},"collection":{"type":"string"},"cursor":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100},"version":{"type":"integer","minimum":1},"scope":scope,"name":{"type":"string","description":"Optional installation name override; not a catalog lookup key. Use id to select a resource."},"values":{"type":"object","additionalProperties":{"type":"string"},"description":"MCP placeholder values."}}),
                vec![
                    "search",
                    "detail",
                    "versions",
                    "collections",
                    "manifest",
                    "installed",
                    "install",
                    "update",
                    "enable",
                    "disable",
                    "remove",
                    "plugin_marketplaces",
                    "add_plugin_marketplace",
                ],
            ),
            ManagementKind::Capabilities => (
                "capability_manage",
                "List this runtime's installed skills, agents, MCP servers, plugins, and extensions. Enable or disable an installed capability without deleting its files. Use marketplace_manage for catalog discovery, installs, versions and updates; use the individual management tools to create or edit custom definitions. Scope local means this runtime workspace; global means all projects on this machine. Changes apply when active work is idle.",
                json!({"kind":{"type":"string","enum":["skill","agent","mcp","plugin","extension"]},"name":{"type":"string"},"scope":scope}),
                vec!["list", "enable", "disable"],
            ),
            ManagementKind::Skill => (
                "skill_manage",
                "Manage local and global Agent Skills. Use this only when the user intends to create, edit, delete, inspect, list, or import skill configuration; do not call it merely because skills are discussed. A skill is a directory containing SKILL.md with name, description, optional notes, and markdown instructions. Plugin-owned skills are read-only here and must be changed through their containing plugin.",
                json!({
                    "name": {"type":"string","description":"Skill name."},
                    "scope": scope,
                    "description": {"type":"string","description":"One-line description for create/edit."},
                    "notes": {"type":"string","description":"Optional setup or access notes for create/edit."},
                    "instructions": {"type":"string","description":"Markdown instructions body for create/edit."},
                    "path": {"type":"string","description":"Skill directory or SKILL.md path for import."}
                }),
                vec!["create", "edit", "delete", "get", "list", "import"],
            ),
            ManagementKind::Plugin => (
                "plugin_manage",
                "Manage local and global Claude-format plugin bundles without using a marketplace. Use this only when the user intends to create, edit, delete, inspect, list, enable, disable, or import plugin configuration; do not call it merely because plugins are discussed. A plugin has .claude-plugin/plugin.json and may provide commands, agents, skills, hooks, MCP definitions, or workflows. Use marketplace_manage to discover, install, update and remove catalog resources.",
                json!({
                    "name": {"type":"string","description":"Plugin name."},
                    "scope": scope,
                    "description": {"type":"string","description":"Plugin description for create/edit."},
                    "notes": {"type":"string","description":"Optional plugin notes for create/edit."},
                    "version": {"type":"string","description":"Plugin version for create/edit; create defaults to 1.0.0."},
                    "path": {"type":"string","description":"Local Claude-format plugin directory or plugin.json path for import."}
                }),
                vec![
                    "create", "edit", "delete", "get", "list", "enable", "disable", "import",
                ],
            ),
            ManagementKind::Mcp => (
                "mcp_manage",
                "Manage local and global MCP server configurations. Use this only when the user intends to add, edit, remove, inspect, list, authenticate, log out, enable, or disable MCP configuration; do not call it merely because MCP is discussed. Direct configuration changes update the live registry, so connected additions are available on the next model step. Plugin-owned MCP definitions are read-only here and must be changed through their containing plugin. Use marketplace_manage to browse and install catalog MCP servers.",
                json!({
                    "name": {"type":"string","description":"MCP server name."},
                    "scope": scope,
                    "type": {"type":"string","enum":["local","remote"],"description":"Server type for add; local requires command and remote requires url."},
                    "command": {"type":"string","description":"Executable for a local MCP server."},
                    "args": {"type":"array","items":{"type":"string"},"description":"Arguments for a local MCP server."},
                    "environment": {"type":"object","additionalProperties":{"type":"string"},"description":"Environment for a local MCP server. A value may name an environment variable as ${NAME} or a stored secret as ${keychain:NAME}; never write a secret itself."},
                    "url": {"type":"string","description":"Streamable HTTP URL for a remote MCP server."},
                    "headers": {"type":"object","additionalProperties":{"type":"string"},"description":"Headers for a remote MCP server. A value may name an environment variable as ${NAME} or a stored secret as ${keychain:NAME}, e.g. Bearer ${keychain:github-token}."},
                    "oauth": {"type":"boolean","description":"Enable or disable generic OAuth discovery for a remote server."},
                    "oauthClientId": {"type":"string","description":"Optional public OAuth client id."},
                    "oauthScopes": {"type":"array","items":{"type":"string"},"description":"OAuth scopes."},
                    "enabled": {"type":"boolean","description":"Initial enabled state for add. Defaults to true."},
                    "apps": {"type":"boolean","description":"Interactive apps (MCP App pages) from this server, for add or edit. Defaults to true. false makes it a plain MCP server: it is not told pages render, its results carry no page and its pages are not served."},
                    "callbackPort": {"type":"integer","minimum":0,"maximum":65535,"description":"Loopback OAuth callback port for auth; 0 selects an ephemeral port."}
                }),
                vec![
                    "add", "edit", "remove", "get", "list", "auth", "logout", "enable", "disable",
                ],
            ),
            ManagementKind::Agent => (
                "agent_manage",
                "Manage local and global custom Agent profiles. Use this only when the user intends to create, edit, delete, inspect, or list agent configuration; do not call it merely because agents are discussed. Agents are markdown files with frontmatter and a system prompt. Built-in and plugin-owned agents are read-only here.",
                json!({
                    "name": {"type":"string","description":"Agent name."},
                    "scope": scope,
                    "description": {"type":"string","description":"When to use this agent for create/edit."},
                    "prompt": {"type":"string","description":"System prompt body for create/edit."},
                    "mode": {"type":"string","enum":["all","primary","subagent"],"description":"Where the agent may be selected."},
                    "tools": {"type":"object","additionalProperties":{"type":"boolean"},"description":"Per-tool enable/disable ceiling."},
                    "externalDirectories": {"type":"array","items":{"type":"string"},"description":"External directory globs to allow. Plain directories are normalized to a trailing /* pattern."},
                    "model": {"type":"string","description":"Optional provider/model override."}
                }),
                vec!["create", "edit", "delete", "get", "list"],
            ),
            ManagementKind::Command => (
                "command_manage",
                "Manage local and global slash-command prompt templates. Use only when the user explicitly asks to create, edit, delete, inspect, or list commands. Changes are stored as markdown and are available to new turns without restarting the server.",
                json!({
                    "name": {"type":"string","description":"Slash-command name, without a leading slash."},
                    "scope": scope,
                    "description": {"type":"string","description":"One-line command description."},
                    "template": {"type":"string","description":"Prompt template. Supports $ARGUMENTS and $1 through $9."},
                    "agent": {"type":"string","description":"Optional agent profile override."},
                    "model": {"type":"string","description":"Optional provider/model override."},
                    "argumentHint": {"type":"string","description":"Optional completion hint such as [file] [focus]."}
                }),
                vec!["create", "edit", "delete", "get", "list"],
            ),
            ManagementKind::MemoryList => (
                "memory_list",
                "List durable project-memory entries stored outside the repository. Results contain names, categories, and short descriptions rather than full bodies.",
                json!({"limit":{"type":"integer","minimum":1,"maximum":100,"description":"Maximum results."}}),
                Vec::new(),
            ),
            ManagementKind::MemoryRead => (
                "memory_read",
                "Read one durable project-memory entry by exact name.",
                json!({"name":{"type":"string","description":"Memory entry name."}}),
                Vec::new(),
            ),
            ManagementKind::MemorySearch => (
                "memory_search",
                "Search durable project memory by case-insensitive text across names, descriptions, categories, and content.",
                json!({
                    "query":{"type":"string","description":"Search query."},
                    "limit":{"type":"integer","minimum":1,"maximum":100,"description":"Maximum results."}
                }),
                Vec::new(),
            ),
            ManagementKind::MemorySave => (
                "memory_save",
                "Save stable facts, decisions, preferences, or reusable project knowledge only when the user asks to retain them. Memory is project-scoped and stored outside the repository. Never store credentials or secrets.",
                json!({
                    "name":{"type":"string","description":"Safe memory entry name."},
                    "type":{"type":"string","enum":["user","feedback","project","reference"],"description":"Memory category; defaults to project."},
                    "description":{"type":"string","description":"Short index description."},
                    "content":{"type":"string","description":"Markdown memory body."}
                }),
                Vec::new(),
            ),
            ManagementKind::MemoryDelete => (
                "memory_delete",
                "Delete one durable project-memory entry when the user asks Haarness to forget it.",
                json!({"name":{"type":"string","description":"Memory entry name."}}),
                Vec::new(),
            ),
            ManagementKind::TaskAbort => (
                "task_abort",
                "Stop one running child-agent task owned by the current session. This cancels only the selected child and its descendants, never the parent or sibling tasks. Use the exact child thread ID returned by task.",
                json!({
                    "taskId": {"type":"string","description":"Exact child thread ID to stop."}
                }),
                vec!["abort"],
            ),
            ManagementKind::SessionManage => (
                "session_manage",
                "Inspect durable sessions without changing them. List recent sessions, search their visible transcript, read a compact item window, or get a digest of status, todos, and child tasks. Prefer digest/read over dumping entire transcripts.",
                json!({
                    "threadId": {"type":"string","description":"Thread UUID or unique UUID prefix. Defaults to the current session for read/digest."},
                    "query": {"type":"string","description":"Search text for list/search."},
                    "includeArchived": {"type":"boolean","description":"Include archived sessions."},
                    "after": {"type":"integer","minimum":0,"description":"Return items after this durable sequence."},
                    "limit": {"type":"integer","minimum":1,"maximum":100,"description":"Maximum results/items."}
                }),
                vec!["list", "search", "read", "digest"],
            ),
            ManagementKind::SessionRename => (
                "session_rename",
                "Rename the current session only when the user explicitly asks. Titles must be a non-empty single line of at most 100 characters.",
                json!({"title":{"type":"string","description":"New session title."}}),
                vec!["rename"],
            ),
            ManagementKind::SessionMessage => (
                "session_message",
                "Send a message to another durable session. If it is active, the message is queued into its current turn; if idle, a new turn starts using that session's saved agent/provider/model identity. Use wait when the caller needs the target to become idle before continuing.",
                json!({
                    "threadId": {"type":"string","description":"Target thread UUID or unique UUID prefix."},
                    "message": {"type":"string","description":"Message to deliver."},
                    "wait": {"type":"boolean","description":"Wait for the target session to become idle after delivery."},
                    "waitForBackground": {"type":"boolean","description":"Also wait for child tasks owned by the target."},
                    "timeoutSeconds": {"type":"integer","minimum":1,"maximum":3600,"description":"Wait timeout; defaults to 120 seconds."}
                }),
                vec!["send"],
            ),
            ManagementKind::SessionWait => (
                "session_wait",
                "Wait until another session is idle, optionally including all child-agent work it owns. This observes server state and does not poll through the model.",
                json!({
                    "threadId": {"type":"string","description":"Target thread UUID or unique UUID prefix."},
                    "waitForBackground": {"type":"boolean","description":"Also require no owned child tasks."},
                    "timeoutSeconds": {"type":"integer","minimum":1,"maximum":3600,"description":"Timeout; defaults to 120 seconds."}
                }),
                vec!["wait"],
            ),
            ManagementKind::AgentSwitch => (
                "agent_switch",
                "Switch the current root session to another primary-capable agent only when the user explicitly requests the transition. The target agent continues in a fresh synthetic turn after this turn closes; do not continue substantive work after a successful switch.",
                json!({
                    "agent": {"type":"string","description":"Target built-in or configured primary-capable agent ID."},
                    "reason": {"type":"string","description":"Short reason/context passed to the target agent."}
                }),
                vec!["switch"],
            ),
        };
        let mut properties = properties.as_object().cloned().unwrap_or_default();
        let action_required = matches!(
            self.kind,
            ManagementKind::Marketplace
                | ManagementKind::Capabilities
                | ManagementKind::Skill
                | ManagementKind::Plugin
                | ManagementKind::Mcp
                | ManagementKind::Agent
                | ManagementKind::Command
                | ManagementKind::SessionManage
        );
        if action_required {
            properties.insert(
                "action".into(),
                json!({
                    "type": "string",
                    "enum": actions,
                    "description": "Management operation to perform."
                }),
            );
        }
        let required = match self.kind {
            ManagementKind::TaskAbort => json!(["taskId"]),
            ManagementKind::MemoryRead | ManagementKind::MemoryDelete => json!(["name"]),
            ManagementKind::MemorySearch => json!(["query"]),
            ManagementKind::MemorySave => json!(["name", "description", "content"]),
            ManagementKind::SessionRename => json!(["title"]),
            ManagementKind::SessionMessage => json!(["threadId", "message"]),
            ManagementKind::SessionWait => json!(["threadId"]),
            ManagementKind::AgentSwitch => json!(["agent"]),
            _ if action_required => json!(["action"]),
            _ => json!([]),
        };
        ToolDefinition {
            name: name.into(),
            description: description.into(),
            input_schema: json!({
                "type": "object",
                "properties": properties,
                "required": required,
                "additionalProperties": false
            }),
        }
    }

    fn side_effectful(&self) -> bool {
        true
    }

    fn side_effectful_for(&self, arguments: &Value) -> bool {
        match self.kind {
            ManagementKind::Marketplace | ManagementKind::Capabilities => !matches!(
                arguments["action"].as_str(),
                Some(
                    "list"
                        | "search"
                        | "detail"
                        | "versions"
                        | "manifest"
                        | "installed"
                        | "collections"
                        | "plugin_marketplaces"
                )
            ),
            ManagementKind::Skill
            | ManagementKind::Plugin
            | ManagementKind::Mcp
            | ManagementKind::Agent
            | ManagementKind::Command => !matches!(
                arguments.get("action").and_then(Value::as_str),
                Some("get" | "list")
            ),
            // Durable memory is deliberately stored outside the workspace;
            // lifecycle/session operations likewise must not manufacture a
            // workspace undo checkpoint.
            ManagementKind::MemoryList
            | ManagementKind::MemoryRead
            | ManagementKind::MemorySearch
            | ManagementKind::MemorySave
            | ManagementKind::MemoryDelete
            | ManagementKind::TaskAbort
            | ManagementKind::SessionManage
            | ManagementKind::SessionRename
            | ManagementKind::SessionMessage
            | ManagementKind::SessionWait
            | ManagementKind::AgentSwitch => false,
        }
    }

    async fn execute(&self, arguments: Value, context: ToolContext) -> CoreResult<ToolOutput> {
        self.registry
            .execute(self.kind.resource(), arguments, context)
            .await
    }
}

fn http_client() -> reqwest::ClientBuilder {
    // Wikimedia and other responsible public sites reject anonymous HTTP
    // library defaults. Identify Haarness with a stable product URL while
    // sending no user or machine-specific data.
    reqwest::Client::builder().user_agent(concat!(
        "haarness/",
        env!("CARGO_PKG_VERSION"),
        " (+https://github.com/haarness/haarness)"
    ))
}

/// Media backends are followed wherever they redirect (a regional endpoint,
/// a CDN), as they always were: the user configured them, and the
/// permission rules judge the generation, not an address. Only webfetch
/// keeps redirects on the address it was allowed to read.
fn media_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        http_client()
            .build()
            .expect("the static media HTTP client configuration is valid")
    })
}

fn web_client() -> reqwest::Client {
    http_client()
        // A redirect off the address asked for is not followed: the fetch returns its
        // address, and fetching that is the model's next call, checked
        // against the permission rules, so an allowed address cannot lead
        // past a denied one. The first URL in `previous` is the one asked for.
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() > 10 {
                attempt.error("too many redirects")
            } else if attempt
                .previous()
                .first()
                .is_some_and(|first| redirect_leaves(first, attempt.url()).is_none())
            {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .expect("the static WebFetch HTTP client configuration is valid")
}

/// How a redirect leaves the address a fetch asked for, or `None` when it
/// stays on it: the same host and port, or the same host moving from http
/// to https on the default ports.
fn redirect_leaves(from: &url::Url, to: &url::Url) -> Option<&'static str> {
    let host = |url: &url::Url| {
        url.host_str()
            .map(|host| host.trim_end_matches('.').to_ascii_lowercase())
    };
    if host(from).is_none() || host(from) != host(to) {
        return Some("which is on another host");
    }
    match (from.scheme(), to.scheme()) {
        (from_scheme, to_scheme) if from_scheme == to_scheme => (from.port_or_known_default()
            != to.port_or_known_default())
        .then_some("which is another port of the same host"),
        ("http", "https") if from.port().is_none() && to.port().is_none() => None,
        ("http", "https") => Some("which is another port of the same host"),
        ("https", "http") => Some("which leaves https for unencrypted http"),
        _ => Some("which uses another scheme"),
    }
}

/// A server can hold the connection open or never finish the body, so a
/// fetch has a deadline and Stop abandons it.
async fn webfetch(
    client: &reqwest::Client,
    url: url::Url,
    raw: bool,
    cancel: &tokio_util::sync::CancellationToken,
    deadline: Duration,
) -> CoreResult<ToolOutput> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(CoreError::Cancelled),
        result = tokio::time::timeout(deadline, fetch_page(client, url, raw)) => {
            result.unwrap_or_else(|_| {
                Err(CoreError::Tool(format!(
                    "The page did not finish loading within {} seconds",
                    deadline.as_secs()
                )))
            })
        }
    }
}

/// Reads `url` for webfetch; `webfetch` bounds it in time and by Stop.
async fn fetch_page(client: &reqwest::Client, url: url::Url, raw: bool) -> CoreResult<ToolOutput> {
    let response = client.get(url.clone()).send().await.map_err(tool_error)?;
    // `web_client` follows redirects on the address asked for only, so a
    // redirect that arrives here leads off it.
    if response.status().is_redirection()
        && let Some(target) = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|location| response.url().join(location).ok())
    {
        return Ok(ToolOutput {
            text: format!(
                "{} redirects (HTTP {}) to {target}, {}, so the redirect was not followed. \
                 Call webfetch with {target} to read it; that fetch is checked against the \
                 permission rules like any other.",
                response.url(),
                response.status().as_u16(),
                redirect_leaves(&url, &target).unwrap_or("which leaves the address asked for"),
            ),
            metadata: json!({"redirect": target.as_str()}),
        });
    }
    let response = response.error_for_status().map_err(tool_error)?;
    let html_content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/html"));
    let Some(body) = read_limited(response, WEBFETCH_MAX_BYTES).await? else {
        tracing::info!(
            host = url.host_str().unwrap_or_default(),
            limit = WEBFETCH_MAX_BYTES,
            "the page is too large to fetch"
        );
        return Err(CoreError::Tool(
            "Response too large (over the 6 MiB webfetch limit)".into(),
        ));
    };
    let mut text = String::from_utf8_lossy(&body).into_owned();
    // HTML pages become markdown so the model reads content, not markup
    // soup; sniff the body when the header is absent.
    let looks_like_html = html_content_type
        || text.trim_start().get(..15).is_some_and(|head| {
            head.to_ascii_lowercase().starts_with("<!doctype html")
                || head.to_ascii_lowercase().starts_with("<html")
        });
    if !raw && looks_like_html {
        if let Some(markdown) = html_to_markdown(&text) {
            text = markdown;
        }
    }
    truncate_output(text).await
}

/// htmd writes the text of `script` and `style` like any other block, so a
/// page's inline JavaScript and CSS would read as its content.
fn html_to_markdown(html: &str) -> Option<String> {
    htmd::HtmlToMarkdown::builder()
        .skip_tags(vec![
            "script", "style", "noscript", "template", "iframe", "object", "embed",
        ])
        .build()
        .convert(html)
        .ok()
}

/// A response body, or `None` when it is larger than `limit` bytes. The
/// declared length is checked first, and a body sent without one (or with a
/// wrong one) is read only up to the limit; dropping the response closes
/// the connection.
async fn read_limited(
    mut response: reqwest::Response,
    limit: usize,
) -> CoreResult<Option<Vec<u8>>> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Ok(None);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(tool_error)? {
        if body.len() + chunk.len() > limit {
            return Ok(None);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Some(body))
}

struct ScheduleTool {
    workspace: PathBuf,
    scheduler: Scheduler,
    trust_project: bool,
}

#[async_trait]
impl Tool for ScheduleTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "schedule".into(),
            description: [
                "Manage durable local Haarness schedules backed by launchd on macOS, systemd --user on Linux, or Windows Task Scheduler.",
                "For work in a Cloud workspace that must run while this device is off, use cloud_schedule instead. Local schedules execute on this device and do not select a Cloud workspace or chat.",
                "Use this when the user asks to create, update, list, inspect history, run, enable, disable, or delete a scheduled task.",
                "Schedules are for a future one-shot run or recurrence. Do not use this merely to start background work now; use task with background=true only when that task's independent-sidecar rules are satisfied.",
                "Actions are create, list, update, delete, enable, disable, run, and history.",
                "Create with either a structured trigger plus prompt, or request in one exact form: 'in N minutes to ...', 'every N minutes to ...', 'every day at 8am to ...', 'every Monday at 8am to ...', or 'on <RFC3339 timestamp> to ...'.",
                "Structured triggers are {type:'interval',seconds:N}, {type:'cron',expression:'five fields'}, or {type:'once',at:'RFC3339'}.",
                "Models must use provider/model form. Scheduled turns are non-interactive, auto-approve ordinary permission prompts, and cannot call the question tool.",
            ]
            .join(" "),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["create", "list", "update", "delete", "enable", "disable", "run", "history"], "description":"Operation to perform. Omit no required fields for the selected operation."},
                    "id": {"type": "string", "description":"Existing schedule id. For operations on one schedule, provide id or name."},
                    "name": {"type": "string", "description":"Schedule name, or existing schedule reference when id is omitted."},
                    "request": {"type": "string", "description":"Exact supported natural-language schedule form. For create, use either request or structured trigger plus prompt."},
                    "prompt": {"type": "string", "description":"Prompt run by the schedule. Required with a structured trigger."},
                    "workspace": {"type": "string", "description":"Workspace used by scheduled turns. Defaults to the current workspace."},
                    "model": {"type": ["string", "null"], "description":"Provider/model identifier. On update, null clears the override."},
                    "agent": {"type": ["string", "null"], "description":"Agent profile override. On update, null clears it."},
                    "variant": {"type": ["string", "null"], "description":"Model variant override. On update, null clears it."},
                    "timezone": {"type": ["string", "null"], "description":"IANA timezone for calendar triggers. On update, null clears it."},
                    "enabled": {"type": "boolean", "description":"Whether the schedule is enabled."},
                    "trigger": {
                        "description":"Structured schedule trigger. Use this with prompt instead of request.",
                        "oneOf": [
                            {"type": "object", "properties": {"type": {"const": "interval"}, "seconds": {"type": "integer", "minimum": 60}}, "required": ["type", "seconds"], "additionalProperties": false},
                            {"type": "object", "properties": {"type": {"const": "cron"}, "expression": {"type": "string"}}, "required": ["type", "expression"], "additionalProperties": false},
                            {"type": "object", "properties": {"type": {"const": "once"}, "at": {"type": "string", "format": "date-time"}}, "required": ["type", "at"], "additionalProperties": false}
                        ]
                    },
                    "limit": {"type": "integer", "minimum": 1, "maximum": 500, "description":"Maximum history records to return. Defaults to 20."}
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        }
    }

    fn side_effectful(&self) -> bool {
        // Scheduler registration is external state, not a workspace mutation.
        false
    }

    async fn execute(&self, arguments: Value, context: ToolContext) -> CoreResult<ToolOutput> {
        if context.cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        let input = arguments
            .as_object()
            .ok_or_else(|| CoreError::Invalid("schedule input must be an object".into()))?;
        let action = input
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| CoreError::Invalid("schedule action is required".into()))?;
        validate_schedule_input(input)?;
        // A chat that works in a folder of its own sees and changes only the
        // schedules made in that folder, and schedules only into it; a chat
        // without one sees no chat's schedules.
        let own_folder = chat_folder(&self.workspace, &context);
        let visible = |schedule: &haarness_scheduler::Schedule| match &own_folder {
            Some(folder) => in_folder(schedule, folder),
            None => schedule.directory.is_none(),
        };
        if own_folder.is_some() && input.get("workspace").is_some_and(|value| !value.is_null()) {
            return Err(CoreError::Invalid(
                "A chat with a folder of its own schedules only into its own folder; leave out workspace.".into(),
            ));
        }
        let reference = || -> CoreResult<String> {
            let reference = input
                .get("id")
                .or_else(|| input.get("name"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    CoreError::Invalid(format!("schedule id or name is required for {action}"))
                })?;
            match self.scheduler.find(reference).map_err(schedule_error)? {
                Some(schedule) if visible(&schedule) => Ok(schedule.id.to_string()),
                // Another chat's schedule reads as no schedule at all.
                _ => Err(CoreError::Tool(format!("schedule not found: {reference}"))),
            }
        };
        match action {
            "list" => {
                let mut schedules = self.scheduler.list().map_err(schedule_error)?;
                schedules.retain(|schedule| visible(schedule));
                schedule_output("Schedules", &schedules)
            }
            "history" => {
                let reference = reference()?;
                let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(20);
                schedule_output(
                    &format!("Schedule history {reference}"),
                    &self
                        .scheduler
                        .history(&reference, usize::try_from(limit).unwrap_or(20))
                        .map_err(schedule_error)?,
                )
            }
            "run" => {
                let reference = reference()?;
                let scheduler = self.scheduler.clone();
                let run = tokio::task::spawn_blocking(move || scheduler.run_now(&reference))
                    .await
                    .map_err(|error| CoreError::Tool(error.to_string()))?
                    .map_err(schedule_error)?;
                schedule_output("Schedule run", &run)
            }
            "delete" => {
                let removed = self
                    .scheduler
                    .remove(&reference()?)
                    .map_err(schedule_error)?;
                schedule_output(&format!("Removed schedule {}", removed.name), &removed)
            }
            "enable" | "disable" => {
                let schedule = self
                    .scheduler
                    .toggle(&reference()?, action == "enable")
                    .map_err(schedule_error)?;
                schedule_output(
                    &format!(
                        "{} schedule {}",
                        if schedule.enabled {
                            "Enabled"
                        } else {
                            "Disabled"
                        },
                        schedule.name
                    ),
                    &schedule,
                )
            }
            "update" => {
                let schedule = self
                    .scheduler
                    .update(&reference()?, schedule_update(input)?)
                    .map_err(schedule_error)?;
                schedule_output(&format!("Updated schedule {}", schedule.name), &schedule)
            }
            "create" => {
                let named = input
                    .get("workspace")
                    .and_then(Value::as_str)
                    .map(PathBuf::from);
                // Made in a chat with a folder of its own (not the project
                // folder), its runs work in that folder too.
                let directory = own_folder.clone();
                let workspace = named.unwrap_or_else(|| self.workspace.clone());
                let schedule = self
                    .scheduler
                    .create(CreateSchedule {
                        name: schedule_string(input, "name"),
                        prompt: schedule_string(input, "prompt"),
                        request: schedule_string(input, "request"),
                        workspace,
                        directory,
                        model: schedule_string(input, "model"),
                        agent: schedule_string(input, "agent"),
                        variant: schedule_string(input, "variant"),
                        trust_project: self.trust_project,
                        enabled: input
                            .get("enabled")
                            .and_then(Value::as_bool)
                            .unwrap_or(true),
                        trigger: trigger(input)?,
                        timezone: schedule_string(input, "timezone"),
                    })
                    .map_err(schedule_error)?;
                schedule_output(&format!("Created schedule {}", schedule.name), &schedule)
            }
            _ => Err(CoreError::Invalid(format!(
                "unknown schedule action: {action}"
            ))),
        }
    }
}

/// Whether `schedule` was made in the chat folder `folder` (canonical).
fn in_folder(schedule: &haarness_scheduler::Schedule, folder: &Path) -> bool {
    schedule
        .directory
        .as_deref()
        .and_then(|directory| directory.canonicalize().ok())
        .is_some_and(|directory| directory == folder)
}

/// The call's working folder when it is a chat's own folder rather than the
/// project folder `project`.
fn chat_folder(project: &Path, context: &ToolContext) -> Option<PathBuf> {
    let project = project.canonicalize().ok()?;
    let folder = context.workspace.canonicalize().ok()?;
    (folder != project).then_some(folder)
}

fn validate_schedule_input(input: &serde_json::Map<String, Value>) -> CoreResult<()> {
    for field in [
        "action",
        "id",
        "name",
        "request",
        "prompt",
        "workspace",
        "model",
        "agent",
        "variant",
        "timezone",
    ] {
        if input
            .get(field)
            .is_some_and(|value| !value.is_string() && !value.is_null())
        {
            return Err(CoreError::Invalid(format!(
                "schedule {field} must be a string or null"
            )));
        }
    }
    if input
        .get("enabled")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(CoreError::Invalid(
            "schedule enabled must be a boolean".into(),
        ));
    }
    if input.get("limit").is_some_and(|value| {
        value
            .as_u64()
            .is_none_or(|value| !(1..=500).contains(&value))
    }) {
        return Err(CoreError::Invalid(
            "schedule limit must be an integer from 1 through 500".into(),
        ));
    }
    Ok(())
}

fn schedule_string(input: &serde_json::Map<String, Value>, field: &str) -> Option<String> {
    input.get(field).and_then(Value::as_str).map(str::to_owned)
}

// Update JSON needs three states: omitted means unchanged, null means clear,
// and a string means replace.
#[allow(clippy::option_option)]
fn optional_string(input: &serde_json::Map<String, Value>, field: &str) -> Option<Option<String>> {
    input
        .contains_key(field)
        .then(|| schedule_string(input, field))
}

fn trigger(input: &serde_json::Map<String, Value>) -> CoreResult<Option<Trigger>> {
    input
        .get("trigger")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| CoreError::Invalid(format!("invalid schedule trigger: {error}")))
}

fn schedule_update(input: &serde_json::Map<String, Value>) -> CoreResult<UpdateSchedule> {
    Ok(UpdateSchedule {
        name: schedule_string(input, "name"),
        prompt: schedule_string(input, "prompt"),
        request: schedule_string(input, "request"),
        workspace: schedule_string(input, "workspace").map(PathBuf::from),
        model: optional_string(input, "model"),
        agent: optional_string(input, "agent"),
        variant: optional_string(input, "variant"),
        trust_project: None,
        enabled: input.get("enabled").and_then(Value::as_bool),
        trigger: trigger(input)?,
        timezone: optional_string(input, "timezone"),
    })
}

fn schedule_error(error: anyhow::Error) -> CoreError {
    CoreError::Tool(error.to_string())
}

fn schedule_output<T: serde::Serialize>(title: &str, value: &T) -> CoreResult<ToolOutput> {
    let value = serde_json::to_value(value).map_err(|error| CoreError::Tool(error.to_string()))?;
    Ok(ToolOutput {
        text: serde_json::to_string_pretty(&value)
            .map_err(|error| CoreError::Tool(error.to_string()))?,
        metadata: json!({"title": title, "schedule": value}),
    })
}

fn skill_catalog_text(infos: &[SkillInfo]) -> String {
    use std::fmt::Write as _;
    if infos.is_empty() {
        return String::new();
    }
    let mut text = String::from(" Available skills:");
    for info in infos
        .iter()
        .filter(|s| !s.execution.disable_model_invocation)
    {
        let _ = write!(text, " {} ({}).", info.name, info.description);
    }
    text
}
impl ToolRegistry for NativeToolRegistry {
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        if let Some(tool) = self.tools.get(name) {
            return Some(tool.clone());
        }
        if let Some(mcp) = self
            .mcp
            .as_ref()
            .filter(|mcp| !mcp.resource_servers().is_empty())
        {
            let [list, read] = crate::mcp_resources::tools(mcp);
            match name {
                crate::mcp_resources::LIST => return Some(list),
                crate::mcp_resources::READ => return Some(read),
                _ => {}
            }
        }
        let definition = self
            .mcp
            .as_ref()?
            .tool_snapshot()
            .into_iter()
            .find(|tool| tool.name == name)?;
        let server = name.strip_prefix("mcp__")?.split_once("__")?.0.to_owned();
        Some(Arc::new(McpProjectedTool {
            definition,
            server,
            registry: self.mcp.as_ref()?.clone(),
        }))
    }
    fn resource_servers(&self) -> Vec<String> {
        self.mcp
            .as_ref()
            .map(|mcp| mcp.resource_servers())
            .unwrap_or_default()
    }
    fn list(&self) -> Vec<ToolDefinition> {
        let mut values: Vec<_> = self.tools.values().map(|tool| tool.definition()).collect();
        if let Some(mcp) = &self.mcp {
            values.extend(mcp.tool_snapshot());
            // Only while a connected server declares resources: most expose
            // tools only, and the two tools would cost every request for
            // nothing.
            if !mcp.resource_servers().is_empty() {
                values.extend(crate::mcp_resources::definitions());
            }
        }
        values.sort_by(|a, b| a.name.cmp(&b.name));
        values
    }
}

struct WorkflowTool {
    coordinator: Arc<WorkflowCoordinator>,
}

#[async_trait]
impl Tool for WorkflowTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "workflow".into(),
            description: [
                "Manage and run durable Haarness workflows.",
                "This model-facing tool is exposed to a primary turn only after explicit workflow wording, explicit tools.workflow=true, or Ultracode activation. A discovered whenToUse description helps select among available workflows; it does not independently activate orchestration.",
                "Do not start a workflow for ordinary conversation, a focused edit, or a short sequential task. At most one top-level workflow should normally be active for a request. Child agents and workflow agents cannot create nested workflows unless their author explicitly grants tools.workflow=true.",
                "Reusable files live under .haarness/workflows/<name>.ts; compatible legacy workflow roots are also discovered.",
                "For a one-off inline workflow, prefer the restricted portable form: export const meta = { name: 'audit', phases: ['scan'] }; phase('scan'); const results = await parallel([() => agent('inspect A'), () => agent('inspect B')], { concurrencyLimit: 2 }); return results. Portable bodies expose args, phase, status, log, agent, tool, shell, workflow, parallel, and pipeline without direct filesystem/process/network globals.",
                "Use read to list workflows or return one source; start with exactly one of name, source, or path; wait/inspect/cancel/pause/resume/delete operate on a runID. name is the invocation read lists: a plugin's workflow is plugin:name.",
                "When source or path is supplied for start, it is authoritative and name is ignored. Do not include name expecting an additional lookup.",
                "start may set workflow-wide agent, model, and variant defaults; an individual agent call and phase model/variant metadata take precedence.",
                "Use create only when the user explicitly asks to persist the inline source. Creation never overwrites an existing file.",
                "Native workflow context supports runID, setPhase, setStatus, log, shell, tool, agent, parallel, pipeline, nested workflow, and agent follow-up/close.",
            ]
            .join(" "),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["read", "start", "wait", "inspect", "create", "cancel", "pause", "resume", "delete"],
                        "description":"read lists workflows or reads one by its invocation (name); start requires exactly one of name, path, or source; wait, inspect, cancel, pause, resume, and delete require runID; create persists source."
                    },
                    "name": {"type": "string", "description":"An existing workflow's invocation, exactly as read lists it, for read or start-by-name: a plugin's workflow is plugin:name (such as shipping:audit), never its bare name. Required with source for create. Omit for start when path or source is supplied."},
                    "args": {"description":"JSON value passed to the workflow. Native workflow arguments must match its declaration."},
                    "runID": {"type": "string", "description":"Exact workflow run UUID returned by start. Required for wait, inspect, cancel, pause, resume, and delete."},
                    "source": {"type": "string", "description":"Inline workflow source for a one-off start or explicit create. For start, use source without name."},
                    "path": {"type": "string", "description":"Workflow TypeScript file path, workspace-relative or absolute, for start. Alternative to name or source."},
                    "agent": {"type": "string", "description":"Workflow-wide agent profile default for start."},
                    "model": {"type": "string", "description":"Workflow-wide provider/model default for start."},
                    "variant": {"type": "string", "description":"Workflow-wide model variant default for start."},
                    "timeout": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Wait timeout in milliseconds (action=wait only). Expiry returns the latest running snapshot; it does not fail or cancel the workflow."
                    },
                    "executionTimeout": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional workflow-wide execution deadline in milliseconds (action=start only). Omit for the normal 30-minute limit; use only when the user requests a deadline."
                    },
                    "overwrite": {"type": "boolean", "const": false, "description":"Workflow creation never overwrites; omit this field or pass false."}
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        }
    }

    fn side_effectful(&self) -> bool {
        // Child workflow turns own any workspace snapshots they need.
        false
    }

    async fn execute(&self, arguments: Value, context: ToolContext) -> CoreResult<ToolOutput> {
        if context.cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        let input = arguments
            .as_object()
            .ok_or_else(|| CoreError::Invalid("workflow input must be an object".into()))?;
        validate_workflow_tool_input(input)?;
        let action = input
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| CoreError::Invalid("workflow action is required".into()))?;
        match action {
            "read" => {
                // The chat's own folder's workflows are this chat's too.
                let catalog = self.coordinator.catalog_for(Some(context.thread_id)).await;
                if let Some(name) = input.get("name").and_then(Value::as_str) {
                    let result = catalog.read_source(name).map_err(workflow_catalog_error)?;
                    Ok(ToolOutput {
                        text: result.source,
                        metadata: json!({"title": format!("Workflow {name}"), "name": name}),
                    })
                } else {
                    workflow_json_output("Workflows", &catalog.list(), json!({}))
                }
            }
            "create" => {
                let name = input.get("name").and_then(Value::as_str).ok_or_else(|| {
                    CoreError::Invalid("name is required for workflow create".into())
                })?;
                let source = input
                    .get("source")
                    .and_then(Value::as_str)
                    .ok_or_else(|| CoreError::Invalid("workflow create needs a source".into()))?;
                if input.get("overwrite").and_then(Value::as_bool) == Some(true) {
                    return Err(CoreError::Conflict(
                        "workflow create never overwrites an existing file".into(),
                    ));
                }
                let result = create_workflow_file(
                    &context.workspace,
                    self.coordinator.catalog().as_ref(),
                    name,
                    source,
                )
                .await?;
                workflow_json_output(&format!("Created workflow {name}"), &result, result.clone())
            }
            "start" => {
                // Inline source/path is authoritative if a
                // caller redundantly includes a name.
                let source = input
                    .get("source")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let path = input
                    .get("path")
                    .and_then(Value::as_str)
                    .map(|value| workflow_path(&context.workspace, value));
                let name = (source.is_none() && path.is_none())
                    .then(|| input.get("name").and_then(Value::as_str).map(str::to_owned))
                    .flatten();
                let timeout_ms = input
                    .get("executionTimeout")
                    .map(|value| {
                        value.as_u64().filter(|value| *value > 0).ok_or_else(|| {
                            CoreError::Invalid(
                                "workflow executionTimeout must be a positive integer".into(),
                            )
                        })
                    })
                    .transpose()?;
                let request = WorkflowStartRequest {
                    name,
                    source,
                    path,
                    args: input.get("args").cloned(),
                    agent: input
                        .get("agent")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    model: input
                        .get("model")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    variant: input
                        .get("variant")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    thread_id: Some(context.thread_id),
                    wait: false,
                    timeout_ms,
                };
                let result = self.coordinator.start(request).await?;
                let guidance = format!(
                    "Started workflow \"{}\" (run {}). It runs in the background; this conversation will be notified automatically when it finishes. Do not sleep or poll it. Use at most one wait/inspect only when its current state is required for your immediate next step; otherwise continue non-overlapping work or end your response.",
                    result.workflow_name, result.id
                );
                workflow_run_output(
                    &format!("Started workflow {}", result.workflow_name),
                    &result,
                    Some(&guidance),
                )
                .await
            }
            "wait" => {
                let run_id = workflow_run_id(input, "wait")?;
                let timeout = input
                    .get("timeout")
                    .map(|value| {
                        value
                            .as_u64()
                            .filter(|value| *value > 0)
                            .map(Duration::from_millis)
                            .ok_or_else(|| {
                                CoreError::Invalid(
                                    "workflow wait timeout must be a positive integer".into(),
                                )
                            })
                    })
                    .transpose()?;
                let claimed = self
                    .coordinator
                    .begin_launch_wait(run_id, context.thread_id);
                let wait = self.coordinator.wait(run_id, timeout);
                tokio::pin!(wait);
                let result = tokio::select! {
                    () = context.cancel.cancelled() => {
                        self.coordinator.end_launch_wait(run_id, claimed, false).await;
                        return Err(CoreError::Cancelled);
                    }
                    result = &mut wait => result,
                };
                let result = match result {
                    Ok(result) => result,
                    Err(error) => {
                        self.coordinator
                            .end_launch_wait(run_id, claimed, false)
                            .await;
                        return Err(error);
                    }
                };
                let terminal = matches!(
                    result.status,
                    haarness_protocol::WorkflowRunStatus::Completed
                        | haarness_protocol::WorkflowRunStatus::Failed
                        | haarness_protocol::WorkflowRunStatus::Cancelled
                );
                self.coordinator
                    .end_launch_wait(run_id, claimed, terminal)
                    .await;
                workflow_run_output(&format!("Workflow {run_id}"), &result, None).await
            }
            "inspect" => {
                let run_id = workflow_run_id(input, "inspect")?;
                let result = self.coordinator.get(run_id, None).await?;
                workflow_run_output(&format!("Workflow {run_id}"), &result, None).await
            }
            "cancel" => {
                let run_id = workflow_run_id(input, "cancel")?;
                let result = self.coordinator.cancel(run_id).await?;
                workflow_run_output(&format!("Cancelled workflow {run_id}"), &result, None).await
            }
            "pause" => {
                let run_id = workflow_run_id(input, "pause")?;
                let result = self.coordinator.pause(run_id).await?;
                workflow_run_output(&format!("Paused workflow {run_id}"), &result, None).await
            }
            "resume" => {
                let run_id = workflow_run_id(input, "resume")?;
                let result = self.coordinator.resume(run_id).await?;
                workflow_run_output(&format!("Resumed workflow {run_id}"), &result, None).await
            }
            "delete" => {
                let run_id = workflow_run_id(input, "delete")?;
                if !self.coordinator.delete(run_id).await? {
                    return Err(CoreError::NotFound(format!("workflow run {run_id}")));
                }
                workflow_json_output(
                    &format!("Deleted workflow {run_id}"),
                    &json!({"id": run_id}),
                    json!({"runID": run_id}),
                )
            }
            _ => Err(CoreError::Invalid(format!(
                "unknown workflow action: {action}"
            ))),
        }
    }
}

/// A workflow file a `start` names: relative to the folder the call works
/// in, or in the home folder for `~/…`, as for the file tools.
fn workflow_path(workspace: &Path, value: &str) -> String {
    let value = haarness_core::expand_home(value);
    let path = Path::new(value.as_ref());
    if path.is_relative() {
        workspace.join(path).display().to_string()
    } else {
        value.into_owned()
    }
}

fn validate_workflow_tool_input(input: &serde_json::Map<String, Value>) -> CoreResult<()> {
    for field in [
        "action", "name", "runID", "source", "path", "agent", "model", "variant",
    ] {
        if input.get(field).is_some_and(|value| !value.is_string()) {
            return Err(CoreError::Invalid(format!(
                "workflow {field} must be a string"
            )));
        }
    }
    if input
        .get("timeout")
        .is_some_and(|value| value.as_u64().filter(|timeout| *timeout > 0).is_none())
    {
        return Err(CoreError::Invalid(
            "workflow timeout must be a positive integer".into(),
        ));
    }
    if input
        .get("executionTimeout")
        .is_some_and(|value| value.as_u64().filter(|timeout| *timeout > 0).is_none())
    {
        return Err(CoreError::Invalid(
            "workflow executionTimeout must be a positive integer".into(),
        ));
    }
    let action = input.get("action").and_then(Value::as_str);
    if action == Some("start") && input.contains_key("timeout") {
        return Err(CoreError::Invalid(
            "workflow timeout is for action=wait; use executionTimeout for an explicit start deadline"
                .into(),
        ));
    }
    if action != Some("start") && input.contains_key("executionTimeout") {
        return Err(CoreError::Invalid(
            "workflow executionTimeout is only valid for action=start".into(),
        ));
    }
    if input
        .get("overwrite")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(CoreError::Invalid(
            "workflow overwrite must be a boolean".into(),
        ));
    }
    Ok(())
}

async fn create_workflow_file(
    workspace: &Path,
    catalog: &haarness_workflow_catalog::WorkflowCatalog,
    name: &str,
    source: &str,
) -> CoreResult<Value> {
    if !safe_workflow_slug(name) {
        return Err(CoreError::Invalid(
            "workflow name must be a lowercase slug beginning with a letter or digit and containing only letters, digits, '-' or '_'"
                .into(),
        ));
    }
    let workspace = tokio::fs::canonicalize(workspace)
        .await
        .map_err(tool_error)?;
    let directory = workspace.join(".haarness/workflows");
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(tool_error)?;
    let canonical_directory = tokio::fs::canonicalize(&directory)
        .await
        .map_err(tool_error)?;
    if !canonical_directory.starts_with(&workspace) {
        return Err(CoreError::PermissionDenied(
            "workflow directory escapes the workspace".into(),
        ));
    }
    let target = canonical_directory.join(format!("{name}.ts"));
    let definition = catalog
        .inspect_source(source, target.display().to_string())
        .map_err(workflow_catalog_error)?;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .await
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                CoreError::Conflict(format!("workflow already exists: {}", target.display()))
            } else {
                tool_error(error)
            }
        })?;
    file.write_all(source.as_bytes())
        .await
        .map_err(tool_error)?;
    file.flush().await.map_err(tool_error)?;
    Ok(json!({
        "path": target,
        "meta": definition.meta,
        "findings": definition.findings,
    }))
}

fn safe_workflow_slug(value: &str) -> bool {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    value.len() <= 128
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && characters.all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '-' | '_')
        })
}

fn workflow_run_id(input: &serde_json::Map<String, Value>, action: &str) -> CoreResult<WorkflowId> {
    let value = input
        .get("runID")
        .and_then(Value::as_str)
        .ok_or_else(|| CoreError::Invalid(format!("runID is required for workflow {action}")))?;
    uuid::Uuid::parse_str(value)
        .map(WorkflowId)
        .map_err(|error| CoreError::Invalid(format!("invalid workflow runID: {error}")))
}

const WORKFLOW_SUMMARY_AGENT_LIMIT: usize = 20;
const WORKFLOW_SUMMARY_LOG_LIMIT: usize = 5;

fn workflow_run_summary(run: &WorkflowRun) -> Value {
    let counts = run.agents.iter().fold([0_u64; 4], |mut counts, agent| {
        let index = match agent.status {
            WorkflowRunAgentStatus::Running => 0,
            WorkflowRunAgentStatus::Completed => 1,
            WorkflowRunAgentStatus::Failed => 2,
            WorkflowRunAgentStatus::Cancelled => 3,
        };
        counts[index] += 1;
        counts
    });
    let agents = run
        .agents
        .iter()
        .take(WORKFLOW_SUMMARY_AGENT_LIMIT)
        .map(|agent| {
            json!({
                "id": agent.id,
                "label": agent.label,
                "agent": agent.agent,
                "phase": agent.phase,
                "status": agent.status,
                "threadID": agent.thread_id,
                "model": agent.model,
                "variant": agent.variant,
                "error": agent.error.as_deref().map(|error| compact_workflow_text(error, 500)),
            })
        })
        .collect::<Vec<_>>();
    let logs = run
        .logs
        .iter()
        .rev()
        .take(WORKFLOW_SUMMARY_LOG_LIMIT)
        .rev()
        .map(|entry| {
            json!({
                "sequence": entry.sequence,
                "time": entry.time,
                "message": compact_workflow_text(&entry.message, 500),
                "phase": entry.phase,
                "source": entry.source,
            })
        })
        .collect::<Vec<_>>();
    let warnings = run
        .warnings
        .iter()
        .rev()
        .take(WORKFLOW_SUMMARY_LOG_LIMIT)
        .rev()
        .map(|warning| {
            json!({
                "time": warning.time,
                "message": compact_workflow_text(&warning.message, 500),
                "phase": warning.phase,
                "requestedModel": warning.requested_model,
                "resolvedModel": warning.resolved_model,
            })
        })
        .collect::<Vec<_>>();
    let terminal = matches!(
        run.status,
        WorkflowRunStatus::Completed
            | WorkflowRunStatus::Failed
            | WorkflowRunStatus::Cancelled
            | WorkflowRunStatus::Interrupted
    );
    json!({
        "runID": run.id,
        "threadID": run.thread_id,
        "workflowName": run.workflow_name,
        "status": run.status,
        "phase": run.phase,
        "time": run.time,
        "agentCounts": {
            "total": run.agents.len(),
            "running": counts[0],
            "completed": counts[1],
            "failed": counts[2],
            "cancelled": counts[3],
        },
        "agents": agents,
        "omittedAgents": run.agents.len().saturating_sub(WORKFLOW_SUMMARY_AGENT_LIMIT),
        "recentLogs": logs,
        "warnings": warnings,
        "omittedWarnings": run.warnings.len().saturating_sub(WORKFLOW_SUMMARY_LOG_LIMIT),
        "result": terminal.then(|| run.result.clone()).flatten(),
        "error": terminal.then(|| run.error.as_deref().map(|error| compact_workflow_text(error, 2_000))).flatten(),
    })
}

fn compact_workflow_text(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let value = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{value}…")
    } else {
        value
    }
}

async fn workflow_run_output(
    title: &str,
    run: &WorkflowRun,
    guidance: Option<&str>,
) -> CoreResult<ToolOutput> {
    let summary = serde_json::to_string_pretty(&workflow_run_summary(run))
        .map_err(|error| CoreError::Storage(error.to_string()))?;
    let text = guidance.map_or(summary.clone(), |guidance| {
        format!("{guidance}\n\n{summary}")
    });
    let mut output = truncate_output(text).await?;
    let mut metadata = match output.metadata {
        Value::Object(object) => object,
        _ => serde_json::Map::new(),
    };
    metadata.insert("title".into(), title.into());
    metadata.insert("runID".into(), json!(run.id));
    metadata.insert("workflowStatus".into(), json!(run.status));
    output.metadata = Value::Object(metadata);
    Ok(output)
}

fn workflow_json_output<T: serde::Serialize>(
    title: &str,
    value: &T,
    mut metadata: Value,
) -> CoreResult<ToolOutput> {
    if let Value::Object(ref mut object) = metadata {
        object.insert("title".into(), title.into());
    }
    Ok(ToolOutput {
        text: serde_json::to_string_pretty(value)
            .map_err(|error| CoreError::Storage(error.to_string()))?,
        metadata,
    })
}

fn workflow_catalog_error(error: haarness_workflow_catalog::CatalogError) -> CoreError {
    match error {
        haarness_workflow_catalog::CatalogError::NotFound(name) => {
            CoreError::NotFound(format!("workflow {name}"))
        }
        haarness_workflow_catalog::CatalogError::Read { path, message } => {
            CoreError::Invalid(format!("workflow {path}: {message}"))
        }
    }
}

struct McpProjectedTool {
    definition: ToolDefinition,
    server: String,
    registry: Arc<dyn McpRegistry>,
}
#[async_trait]
impl Tool for McpProjectedTool {
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }
    fn side_effectful(&self) -> bool {
        // Projected tools may mutate their remote service, but the host cannot
        // attribute that to a local workspace checkpoint.
        false
    }
    async fn execute(&self, arguments: Value, context: ToolContext) -> CoreResult<ToolOutput> {
        // MCP calls have no time limit of their own, so Stop must be able to
        // abandon one that hangs.
        tokio::select! {
            biased;
            () = context.cancel.cancelled() => Err(CoreError::Cancelled),
            result = self.registry.call(&self.server, &self.definition.name, arguments) => result,
        }
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Read,
    ImageGenerate,
    VideoGenerate,
    List,
    Glob,
    Grep,
    Write,
    Edit,
    MultiEdit,
    ApplyPatch,
    Bash,
    WebFetch,
    Question,
    /// Legacy Haarness spelling retained for existing providers/scripts.
    Todo,
    TodoWrite,
    TodoRead,
    Skill,
    Task,
    GetGoal,
    UpdateGoal,
    ProposeGoalCriteria,
}
struct BuiltinTool {
    kind: Kind,
    client: reqwest::Client,
    skills: Option<Arc<dyn SkillRegistry>>,
    skill_catalog: Arc<str>,
    instructions: Arc<InstructionDiscovery>,
}
#[async_trait]
impl Tool for BuiltinTool {
    fn parallel_safe(&self, _arguments: &Value) -> bool {
        // A task delegation is an independent child agent whose own tools
        // are checked on the child's turn; several may run side by side,
        // bounded by execution.subagent_concurrency in the runtime.
        matches!(
            self.kind,
            Kind::Read | Kind::List | Kind::Glob | Kind::Grep | Kind::WebFetch | Kind::Task
        )
    }
    fn validation_schema(&self) -> Value {
        let mut schema = self.definition().input_schema;
        match self.kind {
            Kind::Read => {
                // The workflow host uses the same read permission path when
                // materializing file context, with richer output for media.
                schema["properties"]["_workflowFileContext"] = json!({"type":"boolean"});
            }
            Kind::Bash => {
                // Existing shell clients use this alias; the provider schema
                // continues to teach the canonical `timeout` field.
                schema["properties"]["timeout_ms"] = schema["properties"]["timeout"].clone();
            }
            Kind::Task => {
                // run_subagent already treats blank/invented strings as a new
                // child request. Real IDs still undergo existence/ownership checks.
                schema["properties"]["task_id"] = json!({"type":"string"});
                schema["properties"]["taskId"] = json!({"type":"string"});
                schema["properties"]["subagent_type"] = schema["properties"]["agent"].clone();
                schema["properties"]["model"] = json!({"type":["string","null"]});
            }
            Kind::Todo | Kind::TodoWrite => {
                let field = if matches!(self.kind, Kind::Todo) {
                    "items"
                } else {
                    "todos"
                };
                let mut entries = schema["properties"][field].clone();
                // The durable todo contract supplies a medium priority when omitted.
                entries["items"]["required"] = json!(["content", "status"]);
                schema["properties"]["items"] = entries.clone();
                schema["properties"]["todos"] = entries;
                schema["required"] = json!([]);
                schema["anyOf"] = json!([{"required":["items"]},{"required":["todos"]}]);
            }
            _ => {}
        }
        schema
    }

    fn definition(&self) -> ToolDefinition {
        if matches!(self.kind, Kind::Skill) {
            return ToolDefinition {
                name: "skill".into(),
                description: format!("{SKILL_DESCRIPTION}{}", self.skill_catalog),
                input_schema: json!({"type":"object","properties":{"name":{"type":"string","description":"Exact skill name from the available skills catalog."},"arguments":{"type":"string"}},"required":["name"],"additionalProperties":false}),
            };
        }
        let (name, description, properties, required) = match self.kind {
            Kind::Read => (
                "read",
                READ_DESCRIPTION,
                json!({
                    "path":{"type":"string","description":"Workspace-relative or absolute path to the text, image, PDF, audio, or video file to read."},
                    "offset":{"type":"integer","minimum":0,"description":"Zero-based line offset. Defaults to 0."},
                    "limit":{"type":"integer","minimum":1,"maximum":20000,"description":"Maximum lines to return. Defaults to 2000; one read returns at most 2000 lines or 50 KB."}
                }),
                vec!["path"],
            ),
            Kind::ImageGenerate => (
                "image_generate",
                IMAGE_GENERATE_DESCRIPTION,
                json!({
                    "prompt":{"type":"string","minLength":1,"maxLength":20000,"description":"Visual prompt for the requested image."},
                    "model":{"type":"string","minLength":1,"maxLength":200,"description":"Optional backend model id; defaults to gpt-image-2 for the OpenAI Images endpoint."},
                    "size":{"type":"string","enum":["auto","1024x1024","1536x1024","1024x1536"],"description":"Optional image size supported by the backend."}
                }),
                vec!["prompt"],
            ),
            Kind::VideoGenerate => (
                "video_generate",
                VIDEO_GENERATE_DESCRIPTION,
                json!({
                    "prompt":{"type":"string","minLength":1,"maxLength":20000,"description":"Prompt for the requested video."},
                    "model":{"type":"string","minLength":1,"maxLength":200,"description":"Optional backend model id."},
                    "aspectRatio":{"type":"string","enum":["16:9","9:16"]},
                    "duration":{"type":"integer","minimum":1,"maximum":30}
                }),
                vec!["prompt"],
            ),
            Kind::List => (
                "list",
                LIST_DESCRIPTION,
                json!({"path":{"type":"string","description":"Workspace-relative or absolute directory path to list."}}),
                vec!["path"],
            ),
            Kind::Glob => (
                "glob",
                GLOB_DESCRIPTION,
                json!({
                    "pattern":{"type":"string","description":"Glob pattern matched beneath path, for example '*.rs' or 'src/**/*.ts'."},
                    "path":{"type":"string","description":"Directory to search. Omit this field to use the workspace; do not pass null or the string 'undefined'."}
                }),
                vec!["pattern"],
            ),
            Kind::Grep => (
                "grep",
                GREP_DESCRIPTION,
                json!({
                    "pattern":{"type":"string","description":"Rust regular expression to search for in file contents."},
                    "path":{"type":"string","description":"File or directory to search. Omit this field to use the workspace."}
                }),
                vec!["pattern"],
            ),
            Kind::Write => (
                "write",
                WRITE_DESCRIPTION,
                json!({
                    "path":{"type":"string","description":"Workspace-relative or absolute destination file path."},
                    "content":{"type":"string","description":"Complete UTF-8 contents to write."}
                }),
                vec!["path", "content"],
            ),
            Kind::Edit => (
                "edit",
                EDIT_DESCRIPTION,
                json!({
                    "path":{"type":"string","description":"Workspace-relative or absolute UTF-8 file path to modify."},
                    "old":{"type":"string","description":"Exact text to replace. It must occur exactly once."},
                    "new":{"type":"string","description":"Replacement text."}
                }),
                vec!["path", "old", "new"],
            ),
            Kind::MultiEdit => (
                "multiedit",
                MULTIEDIT_DESCRIPTION,
                json!({
                    "path":{"type":"string","description":"Workspace-relative or absolute UTF-8 file path to modify."},
                    "edits":{"type":"array","minItems":1,"maxItems":100,"items":{"type":"object","properties":{
                        "old":{"type":"string","minLength":1,"description":"Exact text to replace."},
                        "new":{"type":"string","description":"Replacement text."},
                        "replaceAll":{"type":"boolean","default":false,"description":"Replace every occurrence instead of requiring exactly one."}
                    },"required":["old","new"],"additionalProperties":false}}
                }),
                vec!["path", "edits"],
            ),
            Kind::ApplyPatch => (
                "apply_patch",
                APPLY_PATCH_DESCRIPTION,
                json!({"input":{"type":"string","description":"Complete V4A patch text describing every file operation."}}),
                vec!["input"],
            ),
            Kind::Bash => (
                "bash",
                BASH_DESCRIPTION,
                json!({
                    "command":{"type":"string","description":"Shell command to execute."},
                    "timeout":{"type":"integer","minimum":0,"description":"Optional timeout in milliseconds; defaults to the configured deadline (normally 120000). Zero disables the deadline."},
                    "workdir":{"type":"string","description":"Workspace-relative or absolute working directory. Omit to use the workspace; prefer this over a cd prefix."}
                }),
                vec!["command"],
            ),
            Kind::WebFetch => (
                "webfetch",
                WEBFETCH_DESCRIPTION,
                json!({
                    "url":{"type":"string","description":"Fully formed HTTP or HTTPS URL to fetch."},
                    "format":{"type":"string","enum":["markdown","raw"],"default":"markdown","description":"Return converted markdown (default) or the raw response body."}
                }),
                vec!["url"],
            ),
            Kind::Question => (
                "question",
                QUESTION_DESCRIPTION,
                json!({
                    "prompt":{"type":"string","description":"Single concise question shown to the user."},
                    "choices":{"type":"array","items":{"type":"string"},"description":"Optional answer labels. Put a recommended choice first and suffix it with '(Recommended)'. Do not include Other when custom is enabled."},
                    "multiple":{"type":"boolean","default":false,"description":"Allow selection of more than one choice. Defaults to false."},
                    "custom":{"type":"boolean","default":true,"description":"Allow a free-form answer. Defaults to true; set false only when every valid answer is represented by a choice, so false requires at least one choice."}
                }),
                vec!["prompt"],
            ),
            Kind::Todo => (
                "todo",
                "Legacy alias of todowrite. Use todowrite for new calls. This call replaces the current task list and follows the same planning rules: use it for non-trivial multi-step work, keep normally one item in_progress, and update statuses promptly.",
                json!({"items":{"type":"array","items":{"type":"object","properties":{"content":{"type":"string"},"status":{"type":"string","enum":["pending","in_progress","completed","cancelled"]},"priority":{"type":"string","enum":["high","medium","low"]}},"required":["content","status"],"additionalProperties":false}}}),
                vec!["items"],
            ),
            Kind::TodoWrite => (
                "todowrite",
                TODO_DESCRIPTION,
                json!({"todos":{"type":"array","items":{"type":"object","properties":{"content":{"type":"string"},"status":{"type":"string","enum":["pending","in_progress","completed","cancelled"]},"priority":{"type":"string","enum":["high","medium","low"]}},"required":["content","status","priority"],"additionalProperties":false}}}),
                vec!["todos"],
            ),
            Kind::TodoRead => ("todoread", TODO_READ_DESCRIPTION, json!({}), vec![]),
            Kind::Skill => unreachable!("skill definition is built above"),
            Kind::Task => (
                "task",
                TASK_DESCRIPTION,
                json!({
                    "prompt":{"type":"string","description":"Detailed autonomous task instructions, including whether to edit or only investigate and how to verify the result. For concurrent siblings, state an exclusive file/topic boundary and require the child to report cross-scope dependencies instead of auditing a sibling's scope."},
                    "agent":{"type":"string","description":"Subagent profile; defaults to general. Built-ins are build, plan, review, explore, and general; configured custom subagent IDs are also accepted."},
                    "description":{"type":"string","description":"Short human-readable task label."},
                    "task_id":{"type":"string","minLength":1,"pattern":"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[1-8][0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}$","description":"Exact prior Haarness child thread UUID returned by an earlier task call. Omit this field entirely when creating a new child; never invent a label, ULID, empty string, or placeholder such as 'new'."},
                    "background":{"type":"boolean","default":false,"description":TASK_BACKGROUND_DESCRIPTION}
                }),
                vec!["prompt"],
            ),
            Kind::GetGoal => ("get_goal", GET_GOAL_DESCRIPTION, json!({}), vec![]),
            Kind::UpdateGoal => (
                "update_goal",
                UPDATE_GOAL_DESCRIPTION,
                json!({
                    "status":{"type":"string","enum":["complete","blocked"],"description":"Required. complete means the objective and all criteria are proven; blocked means the same blocker recurred for at least three consecutive goal turns. Completing requires next_step_question; blocked does not."},
                    "reason":{"type":"string","description":"One short sentence citing completion evidence or identifying the recurring blocker."},
                    "next_step_question":{
                        "type":"object",
                        "description":"Required when status is complete. Use the user's conversation language. The completion prompt must offer Edit, New, Clear, and Dismiss choices, each with a short label and one-sentence description.",
                        "properties":{
                            "title":{"type":"string","minLength":1},
                            "edit":{"type":"object","properties":{"label":{"type":"string","minLength":1},"description":{"type":"string","minLength":1}},"required":["label","description"],"additionalProperties":false},
                            "new":{"type":"object","properties":{"label":{"type":"string","minLength":1},"description":{"type":"string","minLength":1}},"required":["label","description"],"additionalProperties":false},
                            "clear":{"type":"object","properties":{"label":{"type":"string","minLength":1},"description":{"type":"string","minLength":1}},"required":["label","description"],"additionalProperties":false},
                            "dismiss":{"type":"object","properties":{"label":{"type":"string","minLength":1},"description":{"type":"string","minLength":1}},"required":["label","description"],"additionalProperties":false}
                        },
                        "required":["title","edit","new","clear","dismiss"],
                        "additionalProperties":false
                    },
                    "criteria_evidence":{
                        "type":"array",
                        "description":"Required for status=complete when criteria exist. Provide concrete evidence for every criterion returned by get_goal.",
                        "items":{
                            "type":"object",
                            "properties":{
                                "index":{"type":"integer","minimum":1},
                                "evidence":{"type":"string"}
                            },
                            "required":["index","evidence"],
                            "additionalProperties":false
                        }
                    }
                }),
                vec!["status"],
            ),
            Kind::ProposeGoalCriteria => (
                "propose_goal_criteria",
                PROPOSE_GOAL_CRITERIA_DESCRIPTION,
                json!({
                    "criteria":{"type":"array","minItems":2,"maxItems":5,"description":"Two to five atomic, verifiable criteria in the user's conversation language.","items":{"type":"string","minLength":1}},
                    "prompt":{"type":"string","minLength":1,"description":"Approval question in the user's conversation language; explain that all, a subset, or custom criteria may be chosen."},
                    "label":{"type":"string","minLength":1,"description":"Very short localized header for the approval prompt."}
                }),
                vec!["criteria", "prompt", "label"],
            ),
        };
        let input_schema = json!({
            "type":"object",
            "properties":properties,
            "required":required,
            "additionalProperties":false
        });
        ToolDefinition {
            name: name.into(),
            description: description.into(),
            input_schema,
        }
    }
    fn side_effectful(&self) -> bool {
        matches!(
            self.kind,
            Kind::Write
                | Kind::Edit
                | Kind::MultiEdit
                | Kind::ApplyPatch
                | Kind::Bash
                | Kind::ImageGenerate
                | Kind::VideoGenerate
        )
    }
    fn side_effectful_for(&self, arguments: &Value) -> bool {
        if matches!(self.kind, Kind::Bash)
            && arguments
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(haarness_core::bash_is_obviously_read_only)
        {
            return false;
        }
        self.side_effectful()
    }
    async fn execute(&self, args: Value, context: ToolContext) -> CoreResult<ToolOutput> {
        if context.cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        // A file change holds its files from its first read to its last write.
        let changes = match self.kind {
            Kind::Write => "write",
            Kind::Edit => "edit",
            Kind::MultiEdit => "multiedit",
            Kind::ApplyPatch => "apply_patch",
            _ => "",
        };
        let _locks = crate::file_lock::lock(
            lock_paths(changes, &args, &context.workspace),
            &context.cancel,
        )
        .await?;
        match self.kind {
            Kind::Read => {
                let path = resolve_existing(string(&args, "path")?, &context)?;
                // Workflow `ctx.agent({ files })` must use the same direct
                // read permission/hook path as every other workspace read,
                // but it also needs a race-safe canonical media path and a
                // way to distinguish directories before dispatching `list`.
                // Keep this host-only flag out of the public tool schema so a
                // model cannot turn `read` into an ambient stat primitive.
                if args.get("_workflowFileContext").and_then(Value::as_bool) == Some(true) {
                    let metadata = tokio::fs::metadata(&path).await.map_err(tool_error)?;
                    if metadata.is_dir() {
                        return Ok(ToolOutput {
                            text: String::new(),
                            metadata: json!({
                                "workflowFileContext": "directory",
                                "path": path,
                            }),
                        });
                    }
                    if !metadata.is_file() {
                        return Err(CoreError::Tool(format!(
                            "workflow file context is not a regular file: {}",
                            path.display()
                        )));
                    }
                    if let Some(mime) = workflow_media_mime(&path) {
                        return Ok(ToolOutput {
                            text: String::new(),
                            metadata: json!({
                                "workflowFileContext": "media",
                                "mime": mime,
                                "path": path,
                            }),
                        });
                    }
                }
                if haarness_core::attachments::is_image_source(&path.to_string_lossy()) {
                    return read_image(&path, &context).await;
                }
                if let Some(mime) = workflow_media_mime(&path) {
                    let metadata = tokio::fs::metadata(&path).await.map_err(tool_error)?;
                    if !metadata.is_file() {
                        return Err(CoreError::Tool(format!(
                            "media path is not a regular file: {}",
                            path.display()
                        )));
                    }
                    if metadata.len() > context.execution.media_max_bytes {
                        return Err(CoreError::Tool(format!(
                            "{} is larger than the configured {} byte inline media limit",
                            path.display(),
                            context.execution.media_max_bytes
                        )));
                    }
                    let kind = if mime == "application/pdf" {
                        "PDF"
                    } else if mime.starts_with("image/") {
                        "Image"
                    } else if mime.starts_with("audio/") {
                        "Audio"
                    } else {
                        "Video"
                    };
                    let source = path.display().to_string();
                    return Ok(ToolOutput {
                        text: format!("{kind} read successfully: {source}"),
                        metadata: json!({
                            "title": path.file_name().map_or_else(|| source.clone(), |name| name.to_string_lossy().into_owned()),
                            "path": source,
                            "mime": mime,
                            "size": metadata.len(),
                            "attachments": [path.display().to_string()],
                        }),
                    });
                }
                let offset = usize_value(&args, "offset").unwrap_or(0);
                let limit = usize_value(&args, "limit").map(|limit| limit.min(20_000));
                // A line at a time, off the async workers: a file of any size,
                // or one long line, holds only what is shown.
                let file = path.clone();
                let cancel = context.cancel.clone();
                let read = tokio::task::spawn_blocking(move || {
                    text::read_text(&file, offset, limit, &cancel)
                });
                let mut output = output(tokio::select! {
                    () = context.cancel.cancelled() => return Err(CoreError::Cancelled),
                    read = read => read.map_err(tool_error)??,
                });
                self.instructions
                    .attach(&mut output, &path, &context.workspace, context.thread_id);
                Ok(output)
            }
            Kind::ImageGenerate => {
                generate_media(
                    GeneratedMediaKind::Image,
                    &args,
                    &context.workspace,
                    media_client(),
                    &context.cancel,
                )
                .await
            }
            Kind::VideoGenerate => {
                generate_media(
                    GeneratedMediaKind::Video,
                    &args,
                    &context.workspace,
                    media_client(),
                    &context.cancel,
                )
                .await
            }
            Kind::List => {
                let requested = string(&args, "path")?;
                let path = resolve_existing(requested, &context)?;
                let secrets = SecretFilter::new(&context.workspace, requested, &path, None);
                let mut entries = tokio::fs::read_dir(&path).await.map_err(tool_error)?;
                let mut names = Vec::new();
                let mut skipped = Skipped::default();
                while let Some(entry) = entries.next_entry().await.map_err(tool_error)? {
                    let full = entry.path();
                    // Folders stay listed; a secret-named file, or a link to
                    // one, is left out and only counted.
                    if !full.is_dir()
                        && let Some(hidden) = secrets.hides(&full)
                    {
                        skipped.count(hidden);
                        continue;
                    }
                    names.push(entry.file_name().to_string_lossy().into_owned());
                }
                names.sort();
                Ok(with_skipped(
                    truncate_output(names.join("\n")).await?,
                    skipped,
                    false,
                ))
            }
            Kind::Glob => {
                let requested = args.get("path").and_then(Value::as_str).unwrap_or(".");
                let root = resolve_existing(requested, &context)?;
                if !root.is_dir() {
                    return Err(CoreError::Invalid(format!(
                        "glob path must be a directory: {}",
                        root.display()
                    )));
                }
                let pattern = string(&args, "pattern")?;
                // A `..` would leave the folder the outside-folder check
                // judged; the folder is given as `path` instead.
                if climbs(pattern) {
                    return Err(CoreError::Invalid(
                        "a glob pattern cannot contain `..`; pass the folder to search as path"
                            .into(),
                    ));
                }
                let pattern = root.join(pattern).to_string_lossy().into_owned();
                let workspace = context
                    .workspace
                    .canonicalize()
                    .unwrap_or_else(|_| context.workspace.clone());
                // A file a `read` rule denies is left out like a secret one.
                let rules = context
                    .invoker
                    .as_ref()
                    .and_then(|invoker| invoker.read_rules());
                let secrets = SecretFilter::new(&context.workspace, requested, &root, rules);
                let named = context.workspace.clone();
                let cancel = context.cancel.clone();
                // The walk reads folders synchronously and `**` follows links
                // (one to `/` reaches everything), so it runs off the async
                // workers, stops on Stop and at a budget of matches looked at.
                let walk = tokio::task::spawn_blocking(move || {
                    glob_walk(
                        &pattern,
                        &GlobScope {
                            root,
                            workspace,
                            named,
                        },
                        &secrets,
                        &cancel,
                        GLOB_LOOK_LIMIT,
                    )
                });
                let found = tokio::select! {
                    () = context.cancel.cancelled() => return Err(CoreError::Cancelled),
                    found = walk => found.map_err(tool_error)??,
                };
                let output = with_skipped(
                    truncate_output(found.values.join("\n")).await?,
                    found.skipped,
                    found.partial,
                );
                Ok(if found.stopped {
                    with_stop(output, GLOB_LOOK_LIMIT)
                } else {
                    output
                })
            }
            Kind::Grep => {
                let regex = regex::Regex::new(string(&args, "pattern")?).map_err(tool_error)?;
                let requested = args.get("path").and_then(Value::as_str).unwrap_or(".");
                let root = resolve_existing(requested, &context)?;
                // A file a `read` rule denies is left out like a secret one.
                let rules = context
                    .invoker
                    .as_ref()
                    .and_then(|invoker| invoker.read_rules());
                let secrets = SecretFilter::new(&context.workspace, requested, &root, rules);
                // The walker searches a file it is given whatever its filters
                // say, so a secret-named or denied one, or a link to one, is
                // left out here, before anything reads it.
                if root.is_file()
                    && let Some(hidden) = secrets.hides(&root)
                {
                    return Ok(with_skipped(
                        output(String::new()),
                        Skipped::one(hidden),
                        false,
                    ));
                }
                // The walk reads files synchronously, so it runs off the async
                // workers, as glob's does: a large tree or a slow disk holds
                // none of them, and Stop ends the call at once.
                let workspace = context.workspace.clone();
                let cancel = context.cancel.clone();
                let walk = tokio::task::spawn_blocking(move || {
                    text::grep_walk(&regex, &root, &workspace, &secrets, &cancel)
                });
                let found = tokio::select! {
                    () = context.cancel.cancelled() => return Err(CoreError::Cancelled),
                    found = walk => found.map_err(tool_error)??,
                };
                let mut output = with_skipped(
                    truncate_output(found.matches.join("\n")).await?,
                    found.skipped,
                    found.stopped.is_some(),
                );
                if let Some(stop) = found.stopped {
                    output.text = if output.text.is_empty() {
                        stop.note()
                    } else {
                        format!("{}\n\n{}", output.text, stop.note())
                    };
                }
                Ok(output)
            }
            Kind::Write => {
                let requested_path = string(&args, "path")?;
                let new_contents = string(&args, "content")?;
                let path = resolve_new(requested_path, &context)?;
                let existing = existing_diff_body(&path).await;
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .map_err(tool_error)?;
                }
                tokio::fs::write(&path, new_contents)
                    .await
                    .map_err(tool_error)?;
                Ok(output_with_diffs(
                    format!("wrote {}", path.display()),
                    vec![replacement_diff(requested_path, existing, new_contents)],
                    false,
                ))
            }
            Kind::Edit => {
                let requested_path = string(&args, "path")?;
                let path = resolve_existing(requested_path, &context)?;
                let before = string(&args, "old")?;
                let after = string(&args, "new")?;
                let text = text::read_utf8(&path, "edit").await?;
                let (before, after) = text::crlf_edit(&text, before, after);
                let (before, after) = (before.as_ref(), after.as_ref());
                let count = text.matches(before).count();
                if count != 1 {
                    return Err(CoreError::Tool(format!(
                        "expected exactly one match, found {count}"
                    )));
                }
                let updated = text.replacen(before, after, 1);
                tokio::fs::write(&path, &updated)
                    .await
                    .map_err(tool_error)?;
                Ok(output_with_diffs(
                    format!("updated {}", path.display()),
                    vec![text_file_diff(
                        requested_path,
                        FileDiffStatus::Modified,
                        Some(&text),
                        Some(&updated),
                    )],
                    false,
                ))
            }
            Kind::MultiEdit => {
                let requested_path = string(&args, "path")?;
                let path = resolve_existing(requested_path, &context)?;
                let edits = args
                    .get("edits")
                    .and_then(Value::as_array)
                    .filter(|edits| !edits.is_empty() && edits.len() <= 100)
                    .ok_or_else(|| {
                        CoreError::Invalid("edits must contain 1 to 100 entries".into())
                    })?;
                let original = text::read_utf8(&path, "multiedit").await?;
                let mut updated = original.clone();
                for (index, edit) in edits.iter().enumerate() {
                    let before = string(edit, "old")?;
                    let after = string(edit, "new")?;
                    if before.is_empty() {
                        return Err(CoreError::Invalid(format!(
                            "edit {} old must not be empty",
                            index + 1
                        )));
                    }
                    let (before, after) = text::crlf_edit(&updated, before, after);
                    let (before, after) = (before.as_ref(), after.as_ref());
                    let count = updated.matches(before).count();
                    let replace_all = edit
                        .get("replaceAll")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if count == 0 || (!replace_all && count != 1) {
                        let expectation = if replace_all {
                            "at least one match"
                        } else {
                            "exactly one match"
                        };
                        return Err(CoreError::Tool(format!(
                            "edit {} expected {expectation}, found {count}; file was not changed",
                            index + 1
                        )));
                    }
                    updated = if replace_all {
                        updated.replace(before, after)
                    } else {
                        updated.replacen(before, after, 1)
                    };
                }
                tokio::fs::write(&path, &updated)
                    .await
                    .map_err(tool_error)?;
                Ok(output_with_diffs(
                    format!("applied {} edits to {}", edits.len(), path.display()),
                    vec![text_file_diff(
                        requested_path,
                        FileDiffStatus::Modified,
                        Some(&original),
                        Some(&updated),
                    )],
                    false,
                ))
            }
            Kind::ApplyPatch => {
                let operations = haarness_core::patch::parse_patch(string(&args, "input")?)
                    .map_err(CoreError::Tool)?;
                // A move onto the file itself (the same path, a `./` spelling,
                // another case on a case-insensitive disk) would write the
                // file and then delete it. Refused before anything is written.
                for operation in &operations {
                    if let haarness_core::patch::FileOp::Update {
                        path,
                        move_to: Some(target),
                        ..
                    } = operation
                        && let (Ok(source), Ok(destination)) =
                            (resolve_entry(path, &context), resolve_new(target, &context))
                        && same_entry(
                            &source,
                            &destination,
                            &haarness_core::resolve_permission_entry(&context.workspace, target),
                        )
                    {
                        return Err(CoreError::Invalid(format!(
                            "{path}: *** Move to: {target} names the same file; leave out the Move to line to update it in place"
                        )));
                    }
                }
                // Every operation is worked out before anything is written,
                // so a hunk that does not match leaves every file as it was.
                let several = operations.len() > 1;
                let plan = plan_patch(operations, &context)
                    .await
                    .map_err(|error| unchanged(error, several))?;
                let report = plan
                    .steps
                    .iter()
                    .map(|(line, _)| line.clone())
                    .collect::<Vec<_>>();
                write_patch(plan.steps).await?;
                Ok(output_with_diffs(
                    report.join("\n"),
                    plan.diffs,
                    plan.diffs_truncated,
                ))
            }
            Kind::Bash => {
                let command = string(&args, "command")?;
                let workdir = match args.get("workdir") {
                    Some(value) => {
                        let value = value.as_str().ok_or_else(|| {
                            CoreError::Invalid("bash workdir must be a string".into())
                        })?;
                        let path = resolve_existing(value, &context)?;
                        if !path.is_dir() {
                            return Err(CoreError::Invalid(format!(
                                "bash workdir is not a directory: {value}"
                            )));
                        }
                        path
                    }
                    None => context.workspace.clone(),
                };
                let timeout = args
                    .get("timeout")
                    .or_else(|| args.get("timeout_ms"))
                    .and_then(Value::as_u64)
                    .unwrap_or(context.execution.shell_timeout_ms);
                crate::shell::run(
                    command,
                    &workdir,
                    timeout,
                    &context.cancel,
                    &tool_output_dir(),
                    &context.execution.shell_env,
                    (!context.execution.person_shell).then_some(context.thread_id),
                )
                .await
            }
            Kind::WebFetch => {
                let url = url::Url::parse(string(&args, "url")?).map_err(|error| {
                    CoreError::Invalid(format!("invalid webfetch URL: {error}"))
                })?;
                if !matches!(url.scheme(), "http" | "https") {
                    return Err(CoreError::Invalid(
                        "webfetch URL must use http or https".into(),
                    ));
                }
                if url.host_str().is_none() {
                    return Err(CoreError::Invalid("webfetch URL has no hostname".into()));
                }
                let raw = args.get("format").and_then(serde_json::Value::as_str) == Some("raw");
                webfetch(&self.client, url, raw, &context.cancel, WEBFETCH_TIMEOUT).await
            }
            Kind::Todo | Kind::TodoWrite | Kind::TodoRead => Err(CoreError::Tool(
                "todo state is managed by the runtime task coordinator".into(),
            )),
            Kind::Question => Err(CoreError::Tool(
                "question requires an interactive permission/question reply channel".into(),
            )),
            Kind::Skill => {
                let name = string(&args, "name")?;
                let Some(registry) = &self.skills else {
                    return Err(CoreError::Tool(
                        "no skill registry is configured for this server".into(),
                    ));
                };
                let info = registry.describe(name).await?;
                if info.execution.disable_model_invocation {
                    return Err(CoreError::PermissionDenied(
                        "This skill can only be invoked by the user.".into(),
                    ));
                }
                let body = registry.load(name).await?;
                let (_, body) = haarness_core::definitions::markdown(&body)?;
                let Some(invoker) = context.invoker.as_ref() else {
                    if info.execution.fork
                        || body.contains("!`")
                        || !info.execution.allowed_tools.is_empty()
                        || !info.execution.disallowed_tools.is_empty()
                    {
                        return Err(CoreError::Tool(
                            "This skill needs an active agent turn to apply its execution settings"
                                .into(),
                        ));
                    }
                    return truncate_output(body.to_owned()).await;
                };
                invoker.activate_skill_permissions(&info.execution);
                let body = haarness_core::definitions::prepare(
                    body,
                    args.get("arguments").and_then(Value::as_str).unwrap_or(""),
                    &info.execution,
                    &context.thread_id.to_string(),
                    invoker.as_ref(),
                )
                .await?;
                if info.execution.fork {
                    return invoker.invoke("task", json!({"prompt":body,"description":info.name,
                        "agent":info.execution.agent.as_deref().unwrap_or("general"),"model":info.execution.model})).await;
                }
                truncate_output(format!(
                    "Skill directory: {}\n\n{}",
                    info.execution.directory.as_deref().unwrap_or(""),
                    body
                ))
                .await
            }
            Kind::Task => Err(CoreError::Tool(
                "subagent task scheduling requires the runtime task coordinator".into(),
            )),
            Kind::GetGoal | Kind::UpdateGoal | Kind::ProposeGoalCriteria => Err(CoreError::Tool(
                "goal state is managed by the runtime task coordinator".into(),
            )),
        }
    }
}
/// What a patch leaves at a path once its earlier operations are done: the
/// new text, or nothing.
enum Planned {
    Text(String),
    Removed,
}

/// What a patch's earlier operations leave at each path they touch.
struct PatchFiles {
    files: HashMap<PathBuf, Planned>,
    /// Whether each folder looked at ignores letter case (macOS's and
    /// Windows' disks do by default): `Foo.txt` and `foo.txt` are one file
    /// there.
    folds: parking_lot::Mutex<HashMap<PathBuf, bool>>,
}

impl PatchFiles {
    fn new() -> Self {
        Self {
            files: HashMap::new(),
            folds: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    /// Whether the folder that holds `path` ignores letter case. A new
    /// file's folder may not exist yet; the nearest one that does decides.
    fn folds(&self, path: &Path) -> bool {
        let Some(folder) = path.ancestors().skip(1).find(|folder| folder.is_dir()) else {
            return false;
        };
        *self
            .folds
            .lock()
            .entry(folder.to_path_buf())
            .or_insert_with(|| folds_case(folder))
    }

    /// The path an earlier operation gave the file `path` names, and what it
    /// left there.
    fn find(&self, path: &Path) -> Option<(&PathBuf, &Planned)> {
        self.files.get_key_value(path).or_else(|| {
            if !self.folds(path) {
                return None;
            }
            let folded = fold(path);
            self.files.iter().find(|(key, _)| fold(key) == folded)
        })
    }

    fn get(&self, path: &Path) -> Option<&Planned> {
        self.find(path).map(|(_, planned)| planned)
    }

    /// Whether two paths name one file.
    fn same(&self, left: &Path, right: &Path) -> bool {
        left == right || (fold(left) == fold(right) && self.folds(left))
    }

    fn insert(&mut self, path: PathBuf, planned: Planned) {
        if self.folds(&path) {
            let folded = fold(&path);
            self.files.retain(|key, _| fold(key) != folded);
        }
        self.files.insert(path, planned);
    }
}

fn fold(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

/// Whether `folder` ignores letter case: another letter case of a name in
/// it leads to that entry. Each folder is asked, so a case-sensitive disk
/// mounted inside one that is not counts as what it is; a folder with no
/// name to try asks the folder holding it, on the same disk. When it cannot
/// tell, it says no, and a patch takes names exactly as written, as it
/// always did.
fn folds_case(folder: &Path) -> bool {
    let mut folder = folder.to_path_buf();
    loop {
        if let Some(folds) = names_fold_case(&folder) {
            return folds;
        }
        let Some(parent) = folder.parent().map(Path::to_path_buf) else {
            return false;
        };
        if !same_disk(&folder, &parent) {
            return false;
        }
        folder = parent;
    }
}

/// What the first name with a letter in `folder` says, if it has one.
fn names_fold_case(folder: &Path) -> Option<bool> {
    let entries = std::fs::read_dir(folder).ok()?;
    for entry in entries.flatten().take(64) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let swapped: String = name
            .chars()
            .map(|letter| {
                if letter.is_ascii_lowercase() {
                    letter.to_ascii_uppercase()
                } else {
                    letter.to_ascii_lowercase()
                }
            })
            .collect();
        if swapped == name {
            continue;
        }
        let (named, other) = (folder.join(name), folder.join(swapped));
        #[cfg(unix)]
        return Some(same_link(&named, &other));
        #[cfg(not(unix))]
        return Some(same_file(&named, &other));
    }
    None
}

/// Whether two folders are on one disk (a mount point is not on its
/// parent's).
fn same_disk(left: &Path, right: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        matches!(
            (std::fs::metadata(left), std::fs::metadata(right)),
            (Ok(left), Ok(right)) if left.dev() == right.dev()
        )
    }
    #[cfg(not(unix))]
    {
        left.components().next() == right.components().next()
    }
}

/// The one write a patch operation makes.
enum PatchStep {
    Write {
        file: String,
        path: PathBuf,
        content: String,
    },
    Remove {
        file: String,
        entry: PathBuf,
    },
    /// Checked again just before it writes: a file the patch adds earlier
    /// is on disk only then.
    Move {
        file: String,
        target: String,
        entry: PathBuf,
        destination: PathBuf,
        target_entry: PathBuf,
        content: String,
    },
}

struct PatchPlan {
    /// Each operation's report line and its write, in patch order.
    steps: Vec<(String, PatchStep)>,
    diffs: Vec<FileDiff>,
    diffs_truncated: bool,
}

/// Work out every operation of a patch without writing: each reads a file
/// as the operations before it leave it.
async fn plan_patch(
    operations: Vec<haarness_core::patch::FileOp>,
    context: &ToolContext,
) -> CoreResult<PatchPlan> {
    let mut planned = PatchFiles::new();
    let mut plan = PatchPlan {
        steps: Vec::new(),
        diffs: Vec::new(),
        diffs_truncated: false,
    };
    for operation in operations {
        match operation {
            haarness_core::patch::FileOp::Add { path, content } => {
                let resolved = planned_target(&path, &planned, context)?;
                let existing = planned_body(&planned, &resolved).await;
                push_tool_diff(
                    &mut plan.diffs,
                    replacement_diff(&path, existing, &content),
                    &mut plan.diffs_truncated,
                );
                planned.insert(resolved.clone(), Planned::Text(content.clone()));
                plan.steps.push((
                    format!("added {path}"),
                    PatchStep::Write {
                        file: path,
                        path: resolved,
                        content,
                    },
                ));
            }
            haarness_core::patch::FileOp::Update {
                path,
                move_to,
                hunks,
            } => {
                let gone = || CoreError::Tool(format!("{path}: an earlier operation removes it"));
                if removed_entry(&path, &planned, context) {
                    return Err(gone());
                }
                // A file an earlier operation writes is read as it leaves it.
                let earlier = match resolve_new(&path, context) {
                    Ok(key) => match planned.find(&key) {
                        Some((key, Planned::Text(text))) => Some((key.clone(), text.clone())),
                        Some((_, Planned::Removed)) => return Err(gone()),
                        None => None,
                    },
                    Err(_) => None,
                };
                let (resolved, text) = match earlier {
                    Some(earlier) => earlier,
                    None => {
                        let resolved =
                            resolve_existing(&path, context).map_err(|error| at(&path, error))?;
                        let text = tokio::fs::read_to_string(&resolved)
                            .await
                            .map_err(|error| CoreError::Tool(format!("{path}: {error}")))?;
                        (resolved, text)
                    }
                };
                let updated = haarness_core::patch::apply_hunks(&text, &hunks)
                    .map_err(|error| CoreError::Tool(format!("{path}: {error}")))?;
                if let Some(target) = move_to {
                    // The move takes the source's entry away: a link goes,
                    // not the file it leads to. A file the patch adds earlier
                    // is its own entry.
                    let entry = match resolve_entry(&path, context) {
                        Ok(entry) => entry,
                        Err(_) if planned.get(&resolved).is_some() => resolved.clone(),
                        Err(error) => return Err(at(&path, error)),
                    };
                    let removed = match planned.get(&entry) {
                        Some(Planned::Text(text)) => ExistingDiffBody::Text(text.clone()),
                        _ => entry_diff_body(&entry).await,
                    };
                    let destination = planned_target(&target, &planned, context)?;
                    let target_entry =
                        haarness_core::resolve_permission_entry(&context.workspace, &target);
                    // A file the patch adds earlier is checked here.
                    if planned.same(&destination, &entry)
                        || same_entry(&entry, &destination, &target_entry)
                    {
                        return Err(CoreError::Invalid(format!(
                            "{path}: *** Move to: {target} names the same file; leave out the Move to line to update it in place"
                        )));
                    }
                    let destination_before = planned_body(&planned, &destination).await;
                    push_tool_diff(
                        &mut plan.diffs,
                        deletion_diff(&path, removed),
                        &mut plan.diffs_truncated,
                    );
                    push_tool_diff(
                        &mut plan.diffs,
                        replacement_diff(&target, destination_before, &updated),
                        &mut plan.diffs_truncated,
                    );
                    planned.insert(entry.clone(), Planned::Removed);
                    planned.insert(destination.clone(), Planned::Text(updated.clone()));
                    plan.steps.push((
                        format!("moved {path} → {target}"),
                        PatchStep::Move {
                            file: path,
                            target,
                            entry,
                            destination,
                            target_entry,
                            content: updated,
                        },
                    ));
                } else {
                    push_tool_diff(
                        &mut plan.diffs,
                        text_file_diff(
                            &path,
                            FileDiffStatus::Modified,
                            Some(&text),
                            Some(&updated),
                        ),
                        &mut plan.diffs_truncated,
                    );
                    planned.insert(resolved.clone(), Planned::Text(updated.clone()));
                    plan.steps.push((
                        format!("updated {path}"),
                        PatchStep::Write {
                            file: path,
                            path: resolved,
                            content: updated,
                        },
                    ));
                }
            }
            haarness_core::patch::FileOp::Delete { path } => {
                if removed_entry(&path, &planned, context) {
                    return Err(CoreError::Tool(format!(
                        "{path}: an earlier operation removes it"
                    )));
                }
                // A link is deleted itself, not the file it leads to. A file
                // the patch adds earlier is its own entry.
                let resolved = match resolve_entry(&path, context) {
                    Ok(resolved) => resolved,
                    Err(error) => match resolve_new(&path, context)
                        .ok()
                        .and_then(|key| planned.find(&key))
                    {
                        Some((key, Planned::Text(_))) => key.clone(),
                        _ => return Err(at(&path, error)),
                    },
                };
                let existing = match planned.get(&resolved) {
                    Some(Planned::Text(text)) => ExistingDiffBody::Text(text.clone()),
                    _ => entry_diff_body(&resolved).await,
                };
                push_tool_diff(
                    &mut plan.diffs,
                    deletion_diff(&path, existing),
                    &mut plan.diffs_truncated,
                );
                planned.insert(resolved.clone(), Planned::Removed);
                plan.steps.push((
                    format!("deleted {path}"),
                    PatchStep::Remove {
                        file: path,
                        entry: resolved,
                    },
                ));
            }
        }
    }
    Ok(plan)
}

/// Whether an earlier operation of the patch removes the entry `path` names.
fn removed_entry(path: &str, planned: &PatchFiles, context: &ToolContext) -> bool {
    let entry = haarness_core::resolve_permission_entry(&context.workspace, path);
    matches!(planned.get(&entry), Some(Planned::Removed))
}

/// Where a patch writes `path`: the entry itself when an earlier operation
/// removes it (a link it deletes no longer leads anywhere), else the file
/// the path leads to.
fn planned_target(path: &str, planned: &PatchFiles, context: &ToolContext) -> CoreResult<PathBuf> {
    let entry = haarness_core::resolve_permission_entry(&context.workspace, path);
    if matches!(planned.get(&entry), Some(Planned::Removed)) {
        return ensure_within(entry, context);
    }
    let resolved = resolve_new(path, context)?;
    // Another spelling of a file an earlier operation removes (`README.md`
    // deleted, then `readme.md` added on a disk that ignores letter case) is
    // written as the patch spells it, not under the name it had. A link
    // still leads where it leads.
    let link =
        std::fs::symlink_metadata(&entry).is_ok_and(|metadata| metadata.file_type().is_symlink());
    if !link && matches!(planned.get(&resolved), Some(Planned::Removed)) {
        return ensure_within(entry, context);
    }
    Ok(resolved)
}

/// What `path` holds before an operation, as the earlier ones leave it.
async fn planned_body(planned: &PatchFiles, path: &Path) -> ExistingDiffBody {
    match planned.get(path) {
        Some(Planned::Text(text)) => ExistingDiffBody::Text(text.clone()),
        Some(Planned::Removed) => ExistingDiffBody::Missing,
        None => existing_diff_body(path).await,
    }
}

/// Write a planned patch in order. A write that fails says which operations
/// were already applied.
async fn write_patch(steps: Vec<(String, PatchStep)>) -> CoreResult<()> {
    let mut applied: Vec<String> = Vec::new();
    for (line, step) in steps {
        let note = |message: String| {
            if applied.is_empty() {
                message
            } else {
                format!("{message}; already applied: {}", applied.join(", "))
            }
        };
        let failed = |message: String| CoreError::Tool(note(message));
        match step {
            PatchStep::Write {
                file,
                path,
                content,
            } => write_file(&path, &content)
                .await
                .map_err(|error| failed(format!("could not write {file}: {error}")))?,
            PatchStep::Remove { file, entry } => tokio::fs::remove_file(&entry)
                .await
                .map_err(|error| failed(format!("could not delete {file}: {error}")))?,
            PatchStep::Move {
                file,
                target,
                entry,
                destination,
                target_entry,
                content,
            } => {
                if same_entry(&entry, &destination, &target_entry) {
                    return Err(CoreError::Invalid(note(format!(
                        "{file}: *** Move to: {target} names the same file; leave out the Move to line to update it in place"
                    ))));
                }
                write_file(&destination, &content)
                    .await
                    .map_err(|error| failed(format!("could not write {target}: {error}")))?;
                tokio::fs::remove_file(&entry).await.map_err(|error| {
                    failed(format!(
                        "wrote {target} but could not remove {file}: {error}"
                    ))
                })?;
            }
        }
        applied.push(line);
    }
    Ok(())
}

/// Write `content` to `path`, creating the folders it needs.
async fn write_file(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, content).await
}

/// An I/O failure of one patch operation, naming its file.
fn at(path: &str, error: CoreError) -> CoreError {
    match error {
        CoreError::Tool(message) => CoreError::Tool(format!("{path}: {message}")),
        error => error,
    }
}

/// A patch that failed before writing, saying so when it had other
/// operations.
fn unchanged(error: CoreError, several: bool) -> CoreError {
    const NOTE: &str = "; no file was changed";
    match error {
        CoreError::Tool(message) if several => CoreError::Tool(format!("{message}{NOTE}")),
        CoreError::Invalid(message) if several => CoreError::Invalid(format!("{message}{NOTE}")),
        CoreError::PermissionDenied(message) if several => {
            CoreError::PermissionDenied(format!("{message}{NOTE}"))
        }
        error => error,
    }
}

// Paths resolve where the call works (`ToolContext::workspace`): a chat's own
// folder, or the project folder.
fn resolve_existing(value: &str, context: &ToolContext) -> CoreResult<PathBuf> {
    let path = haarness_core::resolve_permission_path(&context.workspace, value)
        .canonicalize()
        .map_err(tool_error)?;
    ensure_within(path, context)
}
/// A file the tool removes (a delete, the old name of a move): a link
/// there is the entry itself, so only the folder holding it is followed.
fn resolve_entry(value: &str, context: &ToolContext) -> CoreResult<PathBuf> {
    let path = haarness_core::resolve_permission_entry(&context.workspace, value);
    std::fs::symlink_metadata(&path).map_err(tool_error)?;
    ensure_within(path, context)
}
fn resolve_new(value: &str, context: &ToolContext) -> CoreResult<PathBuf> {
    if value.is_empty() {
        return Err(CoreError::Invalid("path cannot be empty".into()));
    }
    let joined = haarness_core::resolve_permission_path(&context.workspace, value);
    if joined.exists() {
        return ensure_within(joined.canonicalize().map_err(tool_error)?, context);
    }
    if std::fs::symlink_metadata(&joined).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(CoreError::PermissionDenied(format!(
            "refusing to write through a dangling symlink: {}",
            joined.display()
        )));
    }
    // `resolve_permission_path` already canonicalized the nearest existing
    // ancestor and rebuilt the missing suffix. Authorize the exact new
    // target (the capability carried by this call), not its parent.
    ensure_within(joined, context)
}
fn ensure_within(path: PathBuf, context: &ToolContext) -> CoreResult<PathBuf> {
    let root = context.workspace.canonicalize().map_err(tool_error)?;
    let output_root = tool_output_dir();
    let is_tool_output = output_root
        .canonicalize()
        .is_ok_and(|output_root| path.starts_with(output_root));
    let is_authorized_external = context.authorized_external_paths.contains(&path);
    if !path.starts_with(&root) && !is_tool_output && !is_authorized_external {
        return Err(CoreError::PermissionDenied(format!(
            "external directory: {}",
            path.display()
        )));
    }
    Ok(path)
}

/// Which files `grep`, `glob` and `list` leave out: those the built-in read
/// rules ask about (`*.env` and `*.env.*`, except `*.env.example` and files
/// inside a `node_modules` folder). An explicit read of one still asks.
struct SecretFilter {
    /// The canonical and the configured workspace. A path inside it is judged
    /// workspace-relative, as search output and a follow-up read write it.
    workspaces: [PathBuf; 2],
    /// The canonical root the tool searches, and the same root as the model
    /// named it, which may run through a link.
    root: PathBuf,
    named_root: PathBuf,
    /// The calling agent's rules, when the search also leaves out files a
    /// `read` rule denies (grep and glob).
    rules: Option<haarness_core::PermissionEngine>,
}

/// Why a search leaves a file out.
#[derive(Clone, Copy)]
enum Hidden {
    /// The built-in read rules ask about it (`.env`).
    Secret,
    /// A configured, agent or inherited `read` rule denies it.
    Denied,
    /// Haarness keeps sign-ins in it (`haarness_core::is_credential_store_path`).
    SignIns,
}

/// How many files a search left out, by why.
#[derive(Default, Clone, Copy)]
struct Skipped {
    secret: usize,
    denied: usize,
    sign_ins: usize,
    /// Glob matches in a linked folder that leads out of the project.
    outside: usize,
}

impl Skipped {
    fn one(hidden: Hidden) -> Self {
        let mut skipped = Self::default();
        skipped.count(hidden);
        skipped
    }

    fn count(&mut self, hidden: Hidden) {
        match hidden {
            Hidden::Secret => self.secret += 1,
            Hidden::Denied => self.denied += 1,
            Hidden::SignIns => self.sign_ins += 1,
        }
    }
}

impl SecretFilter {
    fn new(
        workspace: &Path,
        requested: &str,
        root: &Path,
        rules: Option<haarness_core::PermissionEngine>,
    ) -> Self {
        Self {
            workspaces: [
                workspace
                    .canonicalize()
                    .unwrap_or_else(|_| workspace.to_path_buf()),
                workspace.to_path_buf(),
            ],
            root: root.to_path_buf(),
            // `~/notes` names the home folder, as the tools read it.
            named_root: workspace.join(haarness_core::expand_home(requested).as_ref()),
            rules,
        }
    }

    /// `path` workspace-relative when it is inside the workspace.
    fn relative<'a>(&self, path: &'a Path) -> &'a Path {
        self.workspaces
            .iter()
            .find_map(|workspace| path.strip_prefix(workspace).ok())
            .unwrap_or(path)
    }

    fn judge(&self, path: &Path) -> Option<Hidden> {
        let relative = self.relative(path);
        if haarness_core::is_secret_path(relative) {
            return Some(Hidden::Secret);
        }
        if haarness_core::is_credential_store_path(path) {
            return Some(Hidden::SignIns);
        }
        self.rules
            .as_ref()
            .is_some_and(|rules| {
                rules.evaluate(&format!("read:{}", relative.display()))
                    == haarness_core::PermissionDecision::Deny
            })
            .then_some(Hidden::Denied)
    }

    /// Whether `path`, beneath the searched root, is left out by its own
    /// path or by the path the model named, such as a linked package under
    /// `node_modules` whose real folder holds a `.env`.
    fn names_hidden(&self, path: &Path) -> Option<Hidden> {
        self.judge(path).or_else(|| {
            let rest = path.strip_prefix(&self.root).ok()?;
            self.judge(&if rest.as_os_str().is_empty() {
                self.named_root.clone()
            } else {
                self.named_root.join(rest)
            })
        })
    }

    /// [`Self::names_hidden`], or a link that leads to a file left out.
    fn hides(&self, path: &Path) -> Option<Hidden> {
        self.names_hidden(path).or_else(|| {
            std::fs::canonicalize(path)
                .ok()
                .filter(|target| target != path)
                .and_then(|target| self.judge(&target))
        })
    }
}

/// How many matches a glob looks at, those it leaves out included, before it
/// stops: `**` through a link to `/` or the home folder would otherwise walk
/// everything there.
const GLOB_LOOK_LIMIT: usize = 100_000;

/// What a glob found: its matches, what it left out, and whether it stopped
/// at a limit (`stopped` for [`GLOB_LOOK_LIMIT`]).
#[derive(Default)]
struct GlobFound {
    values: Vec<String>,
    skipped: Skipped,
    partial: bool,
    stopped: bool,
}

/// Where a glob looks: the canonical folder it searches, the canonical
/// workspace, and the workspace as configured, which its output is written
/// relative to.
struct GlobScope {
    root: PathBuf,
    workspace: PathBuf,
    named: PathBuf,
}

/// Walk `pattern` (already joined to the scope's root), looking at no more
/// than `limit` matches. Runs synchronously; the caller keeps it off the async
/// workers.
fn glob_walk(
    pattern: &str,
    scope: &GlobScope,
    secrets: &SecretFilter,
    cancel: &tokio_util::sync::CancellationToken,
    limit: usize,
) -> CoreResult<GlobFound> {
    let mut folders = HashMap::<PathBuf, bool>::new();
    let mut found = GlobFound::default();
    for (looked, path) in glob::glob(pattern)
        .map_err(tool_error)?
        .filter_map(Result::ok)
        .enumerate()
    {
        if cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        if found.values.len() == 10_000 {
            found.partial = true;
            break;
        }
        if looked == limit {
            found.partial = true;
            found.stopped = true;
            break;
        }
        // A part starting with `.`, such as `.*`, also matches `.` and `..`.
        if !path.strip_prefix(&scope.root).is_ok_and(|relative| {
            !relative
                .components()
                .any(|part| part == std::path::Component::ParentDir)
        }) {
            continue;
        }
        // `**` follows linked folders: a match in a folder a link takes out
        // of the project and the searched folder is left out, as a list of
        // that folder would ask first.
        let parent = path.parent().unwrap_or(&scope.root).to_path_buf();
        let inside = *folders.entry(parent).or_insert_with_key(|parent| {
            parent.canonicalize().is_ok_and(|real| {
                real.starts_with(&scope.root) || real.starts_with(&scope.workspace)
            })
        });
        if !inside {
            found.skipped.outside += 1;
            continue;
        }
        // `*` matches dotfiles and `**` follows linked folders, so each file
        // is checked by where it really leads as well.
        if !path.is_dir()
            && let Some(hidden) = secrets.hides(&path)
        {
            found.skipped.count(hidden);
            continue;
        }
        found.values.push(
            path.strip_prefix(&scope.named)
                .unwrap_or(&path)
                .display()
                .to_string(),
        );
    }
    Ok(found)
}

/// Ends a glob's output saying it stopped at its budget, so an empty or short
/// list is not read as everything there is.
fn with_stop(mut output: ToolOutput, limit: usize) -> ToolOutput {
    let note =
        format!("(stopped after looking at {limit} matches; narrow the pattern or the path)");
    output.text = if output.text.is_empty() {
        note
    } else {
        format!("{}\n\n{note}", output.text)
    };
    output
}

/// Whether a glob pattern has a `..` part.
fn climbs(pattern: &str) -> bool {
    pattern.split(['/', '\\']).any(|part| part == "..")
}

/// Whether the entry a move removes is the file it writes. A link is its
/// own entry: moving it onto the file it leads to writes that file and
/// removes only the link.
fn same_entry(entry: &Path, destination: &Path, target_entry: &Path) -> bool {
    if std::fs::symlink_metadata(entry).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        // The move would write the file the link leads to and then remove
        // the link: refused when it names the link itself (`L`, `./L`,
        // another case on a case-insensitive disk).
        return same_link(entry, target_entry);
    }
    same_file(entry, destination)
}

/// Whether two entries are the same, without following a link at either.
fn same_link(left: &Path, right: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if let (Ok(left), Ok(right)) = (
            std::fs::symlink_metadata(left),
            std::fs::symlink_metadata(right),
        ) {
            return left.dev() == right.dev() && left.ino() == right.ino();
        }
        false
    }
    #[cfg(not(unix))]
    {
        left == right
            || left
                .to_string_lossy()
                .eq_ignore_ascii_case(&right.to_string_lossy())
    }
}

/// Whether two paths name one existing file: the same device and inode on
/// Unix, the same canonical path elsewhere (Windows canonicalizes case too).
fn same_file(left: &Path, right: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if let (Ok(left), Ok(right)) = (std::fs::metadata(left), std::fs::metadata(right)) {
            return left.dev() == right.dev() && left.ino() == right.ino();
        }
        false
    }
    #[cfg(not(unix))]
    {
        matches!(
            (left.canonicalize(), right.canonicalize()),
            (Ok(left), Ok(right)) if left == right
        )
    }
}

/// Ends a search's output with how many files it left out and why, and
/// records the counts as `skipped` (secret-named) and `denied` (a `read` rule
/// refuses them). The note follows any truncation so the model always sees
/// it; `partial` means the search stopped at its result limit, so files it
/// never reached went uncounted.
fn with_skipped(mut output: ToolOutput, skipped: Skipped, partial: bool) -> ToolOutput {
    let at_least = if partial { "at least " } else { "" };
    let plural = |count: usize| if count == 1 { "" } else { "s" };
    let mut reasons = Vec::new();
    if skipped.secret > 0 {
        reasons.push(format!(
            "{at_least}{} file{} with a secret's name",
            skipped.secret,
            plural(skipped.secret)
        ));
    }
    if skipped.denied > 0 {
        reasons.push(format!(
            "{at_least}{} file{} a read rule denies",
            skipped.denied,
            plural(skipped.denied)
        ));
    }
    if skipped.sign_ins > 0 {
        reasons.push(format!(
            "{at_least}{} file{} where Haarness keeps sign-ins",
            skipped.sign_ins,
            plural(skipped.sign_ins)
        ));
    }
    // Said so the model does not take those files for absent.
    if skipped.outside > 0 {
        reasons.push(format!(
            "{at_least}{} match{} inside links that lead outside the project",
            skipped.outside,
            if skipped.outside == 1 { "" } else { "es" }
        ));
    }
    if reasons.is_empty() {
        return output;
    }
    let note = format!("(skipped {})", reasons.join(" and "));
    output.text = if output.text.is_empty() {
        format!("No files found\n\n{note}")
    } else {
        format!("{}\n\n{note}", output.text)
    };
    if !output.metadata.is_object() {
        output.metadata = json!({});
    }
    if let Value::Object(metadata) = &mut output.metadata {
        if skipped.secret > 0 {
            metadata.insert("skipped".into(), json!(skipped.secret));
        }
        if skipped.denied > 0 {
            metadata.insert("denied".into(), json!(skipped.denied));
        }
        if skipped.sign_ins > 0 {
            metadata.insert("signIns".into(), json!(skipped.sign_ins));
        }
        if skipped.outside > 0 {
            metadata.insert("outside".into(), json!(skipped.outside));
        }
    }
    output
}

fn string<'a>(value: &'a Value, key: &str) -> CoreResult<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| CoreError::Invalid(format!("missing string {key}")))
}
fn usize_value(value: &Value, key: &str) -> Option<usize> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

/// An image file as the model sees it. One every provider reads stays a path
/// to the file; a converted or shrunk one travels inline, since the file
/// itself is not what the provider would accept. Bytes that are no image fail
/// the call with the reason, so nothing unreadable reaches the transcript.
async fn read_image(path: &Path, context: &ToolContext) -> CoreResult<ToolOutput> {
    let metadata = tokio::fs::metadata(path).await.map_err(tool_error)?;
    if !metadata.is_file() {
        return Err(CoreError::Tool(format!(
            "media path is not a regular file: {}",
            path.display()
        )));
    }
    if metadata.len() > context.execution.media_max_bytes {
        return Err(CoreError::Tool(format!(
            "{} is larger than the configured {} byte inline media limit",
            path.display(),
            context.execution.media_max_bytes
        )));
    }
    let bytes = tokio::fs::read(path).await.map_err(tool_error)?;
    let source = path.display().to_string();
    let name = path.file_name().map_or_else(
        || source.clone(),
        |name| name.to_string_lossy().into_owned(),
    );
    let label = name.clone();
    let prepared = tokio::task::spawn_blocking(move || haarness_core::images::prepare(bytes))
        .await
        .map_err(tool_error)?
        .map_err(|refusal| CoreError::Tool(refusal.message(&label)))?;
    let (text, attachment) = if prepared.changed {
        (
            format!(
                "Image read successfully: {source} (shown to you as a {}×{} {})",
                prepared.width,
                prepared.height,
                prepared.extension().to_ascii_uppercase()
            ),
            format!(
                "data:{};name={};base64,{}",
                prepared.mime_type,
                url::form_urlencoded::byte_serialize(name.as_bytes()).collect::<String>(),
                STANDARD.encode(&prepared.bytes)
            ),
        )
    } else {
        (format!("Image read successfully: {source}"), source.clone())
    };
    Ok(ToolOutput {
        text,
        metadata: json!({
            "title": name,
            "path": source,
            "mime": prepared.mime_type,
            "size": metadata.len(),
            "attachments": [attachment],
        }),
    })
}

/// Media formats accepted by Haarness's native provider adapters. Keep this
/// conservative and extension-based: the provider still performs size and
/// content loading checks immediately before its wire request.
fn workflow_media_mime(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        "heic" => Some("image/heic"),
        "heif" => Some("image/heif"),
        "pdf" => Some("application/pdf"),
        "mp3" => Some("audio/mpeg"),
        "wav" => Some("audio/wav"),
        "flac" => Some("audio/flac"),
        "m4a" => Some("audio/mp4"),
        "aac" => Some("audio/aac"),
        "ogg" | "oga" | "opus" => Some("audio/ogg"),
        "mp4" | "m4v" => Some("video/mp4"),
        "mpeg" => Some("video/mpeg"),
        "mov" => Some("video/mov"),
        "avi" => Some("video/avi"),
        "flv" => Some("video/x-flv"),
        "mpg" => Some("video/mpg"),
        "webm" => Some("video/webm"),
        "wmv" => Some("video/wmv"),
        "3gp" | "3gpp" => Some("video/3gpp"),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GeneratedMediaKind {
    Image,
    Video,
}

impl GeneratedMediaKind {
    fn label(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
        }
    }

    fn default_mime(self) -> &'static str {
        match self {
            Self::Image => "image/png",
            Self::Video => "video/mp4",
        }
    }
}

struct MediaEndpoint {
    url: String,
    api_key: Option<String>,
}

async fn generate_media(
    kind: GeneratedMediaKind,
    args: &Value,
    workspace: &Path,
    client: &reqwest::Client,
    cancel: &tokio_util::sync::CancellationToken,
) -> CoreResult<ToolOutput> {
    let endpoint = media_endpoint(kind)?;
    generate_media_from_endpoint(kind, args, workspace, client, cancel, endpoint).await
}

fn media_endpoint(kind: GeneratedMediaKind) -> CoreResult<MediaEndpoint> {
    let configured = match kind {
        GeneratedMediaKind::Image => std::env::var("HAARNESS_IMAGE_GENERATE_URL").ok(),
        GeneratedMediaKind::Video => std::env::var("HAARNESS_VIDEO_GENERATE_URL").ok(),
    };
    let (url, implicit_openai) = match (kind, configured) {
        (_, Some(url)) => (url, false),
        (GeneratedMediaKind::Image, None) if std::env::var_os("OPENAI_API_KEY").is_some() => {
            let base = std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".into());
            (
                format!(
                    "{}/images/generations",
                    base.trim_end_matches('/').trim_end_matches("/responses")
                ),
                true,
            )
        }
        (GeneratedMediaKind::Image, None) => {
            return Err(CoreError::Tool(
                "image generation is not configured; set HAARNESS_IMAGE_GENERATE_URL or OPENAI_API_KEY"
                    .into(),
            ));
        }
        (GeneratedMediaKind::Video, None) => {
            return Err(CoreError::Tool(
                "video generation is not configured; set HAARNESS_VIDEO_GENERATE_URL".into(),
            ));
        }
    };
    let parsed = reqwest::Url::parse(&url)
        .map_err(|error| CoreError::Invalid(format!("invalid media generation URL: {error}")))?;
    let safe_http = parsed.scheme() == "http"
        && parsed
            .host_str()
            .is_some_and(|host| matches!(host, "127.0.0.1" | "::1" | "localhost"));
    if parsed.scheme() != "https" && !safe_http {
        return Err(CoreError::Invalid(
            "media generation URL must use HTTPS, or HTTP on an exact loopback host".into(),
        ));
    }
    Ok(MediaEndpoint {
        url,
        api_key: select_media_api_key(
            implicit_openai,
            std::env::var("HAARNESS_MEDIA_API_KEY").ok(),
            std::env::var("OPENAI_API_KEY").ok(),
        ),
    })
}

fn select_media_api_key(
    implicit_openai: bool,
    media_key: Option<String>,
    openai_key: Option<String>,
) -> Option<String> {
    media_key.or_else(|| implicit_openai.then_some(openai_key).flatten())
}

async fn generate_media_from_endpoint(
    kind: GeneratedMediaKind,
    args: &Value,
    workspace: &Path,
    client: &reqwest::Client,
    cancel: &tokio_util::sync::CancellationToken,
    endpoint: MediaEndpoint,
) -> CoreResult<ToolOutput> {
    let prompt = string(args, "prompt")?.trim();
    if prompt.is_empty() || prompt.len() > 20_000 {
        return Err(CoreError::Invalid(
            "prompt must contain 1 to 20000 characters".into(),
        ));
    }
    let default_model = if kind == GeneratedMediaKind::Image {
        "gpt-image-2"
    } else {
        "default"
    };
    let model = args
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(default_model);
    let mut body = json!({"model":model,"prompt":prompt});
    match kind {
        GeneratedMediaKind::Image => {
            body["output_format"] = json!("png");
            if let Some(size) = args.get("size").and_then(Value::as_str) {
                body["size"] = json!(size);
            }
        }
        GeneratedMediaKind::Video => {
            if let Some(ratio) = args.get("aspectRatio").and_then(Value::as_str) {
                body["aspect_ratio"] = json!(ratio);
            }
            if let Some(duration) = args.get("duration").and_then(Value::as_u64) {
                body["duration"] = json!(duration);
            }
        }
    }
    let mut request = client.post(&endpoint.url).json(&body);
    if let Some(api_key) = endpoint.api_key {
        request = request.bearer_auth(api_key);
    }
    let response = tokio::select! {
        () = cancel.cancelled() => return Err(CoreError::Cancelled),
        response = request.send() => response.map_err(tool_error)?,
    };
    let status = response.status();
    let bytes = tokio::select! {
        () = cancel.cancelled() => return Err(CoreError::Cancelled),
        bytes = read_limited(response, MAX_GENERATION_RESPONSE_BYTES) => bytes?,
    };
    let Some(bytes) = bytes else {
        return Err(CoreError::Tool(
            "media generation response exceeds the 72 MiB transport limit".into(),
        ));
    };
    if !status.is_success() {
        let detail = String::from_utf8_lossy(&bytes);
        return Err(CoreError::Tool(format!(
            "{} generation backend returned {status}: {}",
            kind.label(),
            detail.chars().take(1000).collect::<String>()
        )));
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| CoreError::Tool(format!("invalid media generation JSON: {error}")))?;
    let candidate = value
        .get("data")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .unwrap_or(&value);
    let encoded = candidate
        .get("b64_json")
        .or_else(|| candidate.get("base64"))
        .or_else(|| candidate.get("data"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CoreError::Tool(
                "media generation response must contain b64_json, base64, or data".into(),
            )
        })?;
    let (declared_mime, encoded) = encoded
        .strip_prefix("data:")
        .and_then(|data| data.split_once(','))
        .map_or((None, encoded), |(metadata, payload)| {
            (metadata.split(';').next(), payload)
        });
    let mime = candidate
        .get("mime")
        .or_else(|| candidate.get("mime_type"))
        .or_else(|| candidate.get("mimeType"))
        .or_else(|| value.get("mime"))
        .or_else(|| value.get("mime_type"))
        .or_else(|| value.get("mimeType"))
        .and_then(Value::as_str)
        .or(declared_mime)
        .unwrap_or_else(|| kind.default_mime());
    let extension = generated_media_extension(kind, mime)?;
    if encoded.len() > MAX_GENERATION_RESPONSE_BYTES {
        return Err(CoreError::Tool(
            "encoded generated media exceeds the transport limit".into(),
        ));
    }
    let decoded = STANDARD
        .decode(encoded)
        .map_err(|error| CoreError::Tool(format!("invalid generated media base64: {error}")))?;
    if decoded.is_empty() || decoded.len() > MAX_GENERATED_MEDIA_BYTES {
        return Err(CoreError::Tool(format!(
            "generated {} must contain 1 byte to 50 MiB",
            kind.label()
        )));
    }
    validate_generated_media(kind, mime, &decoded)?;
    let directory = workspace.join(".haarness-chat-attachments");
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(tool_error)?;
    let ignore = directory.join(".gitignore");
    if !ignore.exists() {
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        if let Ok(mut file) = options.open(&ignore).await {
            file.write_all(b"*\n").await.map_err(tool_error)?;
        }
    }
    let stem = media_slug(prompt, kind.label());
    let path = directory.join(format!(
        "{stem}-{}.{}",
        uuid::Uuid::now_v7().simple(),
        extension
    ));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options.open(&path).await.map_err(tool_error)?;
    file.write_all(&decoded).await.map_err(tool_error)?;
    file.flush().await.map_err(tool_error)?;
    let source = path.display().to_string();
    Ok(ToolOutput {
        text: format!("Generated {source}"),
        metadata: json!({
            "title": format!("{} generated", kind.label()),
            "path": source,
            "mime": mime,
            "size": decoded.len(),
            "model": model,
            "attachments": [path.display().to_string()],
        }),
    })
}

fn generated_media_extension(kind: GeneratedMediaKind, mime: &str) -> CoreResult<&'static str> {
    let extension = match (kind, mime) {
        (GeneratedMediaKind::Image, "image/png") => "png",
        (GeneratedMediaKind::Image, "image/jpeg") => "jpg",
        (GeneratedMediaKind::Image, "image/webp") => "webp",
        (GeneratedMediaKind::Image, "image/gif") => "gif",
        (GeneratedMediaKind::Video, "video/mp4") => "mp4",
        (GeneratedMediaKind::Video, "video/webm") => "webm",
        _ => {
            return Err(CoreError::Tool(format!(
                "generation backend returned unsupported {} MIME type {mime}",
                kind.label()
            )));
        }
    };
    Ok(extension)
}

fn validate_generated_media(kind: GeneratedMediaKind, mime: &str, bytes: &[u8]) -> CoreResult<()> {
    let valid = match mime {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP",
        "video/mp4" => bytes.len() >= 8 && &bytes[4..8] == b"ftyp",
        "video/webm" => bytes.starts_with(b"\x1a\x45\xdf\xa3"),
        _ => false,
    };
    if !valid {
        return Err(CoreError::Tool(format!(
            "generation backend returned bytes that do not match declared {} MIME type {mime}",
            kind.label()
        )));
    }
    Ok(())
}

fn media_slug(prompt: &str, fallback: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for character in prompt.chars() {
        if character.is_ascii_alphanumeric() {
            if separator && !slug.is_empty() && slug.len() < 48 {
                slug.push('-');
            }
            separator = false;
            if slug.len() < 48 {
                slug.push(character.to_ascii_lowercase());
            }
        } else {
            separator = true;
        }
        if slug.len() >= 48 {
            break;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        fallback.into()
    } else {
        slug
    }
}

fn output(text: String) -> ToolOutput {
    ToolOutput {
        text,
        metadata: Value::Null,
    }
}

/// Tool result metadata is persisted in the transcript, so cap both the
/// number of files and each before/after body. The full workspace state lives
/// in snapshots; this payload exists only to render an immediate review.
const TOOL_DIFF_MAX_FILES: usize = 64;
const TOOL_DIFF_BODY_MAX_BYTES: usize = 32 * 1_024;
const TOOL_DIFF_LINE_BUDGET: Duration = Duration::from_millis(500);

enum ExistingDiffBody {
    Missing,
    Text(String),
    /// The previous contents were binary or could not be read without
    /// changing the mutation tool's established success/failure behavior.
    Opaque {
        binary: bool,
    },
}

async fn existing_diff_body(path: &Path) -> ExistingDiffBody {
    match tokio::fs::read(path).await {
        Ok(bytes) => String::from_utf8(bytes).map_or(
            ExistingDiffBody::Opaque { binary: true },
            ExistingDiffBody::Text,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ExistingDiffBody::Missing,
        Err(_) => ExistingDiffBody::Opaque { binary: false },
    }
}

/// What a removed entry held: a link's target path (as Git shows a link),
/// otherwise the file's contents.
async fn entry_diff_body(path: &Path) -> ExistingDiffBody {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => tokio::fs::read_link(path)
            .await
            .map_or(ExistingDiffBody::Opaque { binary: false }, |target| {
                ExistingDiffBody::Text(target.to_string_lossy().into_owned())
            }),
        _ => existing_diff_body(path).await,
    }
}

fn replacement_diff(file: &str, before: ExistingDiffBody, after: &str) -> FileDiff {
    match before {
        ExistingDiffBody::Missing => text_file_diff(file, FileDiffStatus::Added, None, Some(after)),
        ExistingDiffBody::Text(before) => {
            text_file_diff(file, FileDiffStatus::Modified, Some(&before), Some(after))
        }
        ExistingDiffBody::Opaque { binary } => {
            opaque_file_diff(file, FileDiffStatus::Modified, None, Some(after), binary)
        }
    }
}

fn deletion_diff(file: &str, before: ExistingDiffBody) -> FileDiff {
    match before {
        ExistingDiffBody::Text(before) => {
            text_file_diff(file, FileDiffStatus::Deleted, Some(&before), None)
        }
        ExistingDiffBody::Missing => {
            opaque_file_diff(file, FileDiffStatus::Deleted, None, None, false)
        }
        ExistingDiffBody::Opaque { binary } => {
            opaque_file_diff(file, FileDiffStatus::Deleted, None, None, binary)
        }
    }
}

fn opaque_file_diff(
    file: &str,
    status: FileDiffStatus,
    before: Option<&str>,
    after: Option<&str>,
    binary: bool,
) -> FileDiff {
    let mut diff = text_file_diff(file, status, before, after);
    diff.binary = binary;
    // A body is necessarily missing when the old bytes cannot be represented
    // as UTF-8 or could not be read. Keep that limitation explicit on the wire.
    diff.truncated = true;
    diff
}

fn text_file_diff(
    file: &str,
    status: FileDiffStatus,
    before: Option<&str>,
    after: Option<&str>,
) -> FileDiff {
    let (additions, deletions) = line_changes(status, before, after);
    let (before, before_truncated) = before.map_or((None, false), |body| {
        let (body, truncated) = bounded_diff_body(body);
        (Some(body), truncated)
    });
    let (after, after_truncated) = after.map_or((None, false), |body| {
        let (body, truncated) = bounded_diff_body(body);
        (Some(body), truncated)
    });
    FileDiff {
        file: file.to_owned(),
        status,
        additions,
        deletions,
        patch: None,
        before,
        after,
        binary: false,
        truncated: before_truncated || after_truncated,
    }
}

fn bounded_diff_body(body: &str) -> (String, bool) {
    if body.len() <= TOOL_DIFF_BODY_MAX_BYTES {
        return (body.to_owned(), false);
    }
    let mut end = TOOL_DIFF_BODY_MAX_BYTES;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    (body[..end].to_owned(), true)
}

/// Lines added and removed by a line diff, the same count `git diff` and the
/// Review panel give. Unchanged lines inside an edited region are not
/// changes: a replacement that keeps four of its lines adds and removes the
/// lines around them, not those four. The deadline keeps a pathological
/// rewrite from holding the tool host; past it the diff is still correct,
/// only possibly not minimal.
fn line_changes(status: FileDiffStatus, before: Option<&str>, after: Option<&str>) -> (u64, u64) {
    let before = before.unwrap_or_default();
    let after = after.unwrap_or_default();
    match status {
        FileDiffStatus::Added => (line_count(after), 0),
        FileDiffStatus::Deleted => (0, line_count(before)),
        FileDiffStatus::Modified => {
            if before == after {
                return (0, 0);
            }
            // Lines keep their terminators here, so a changed final newline
            // still counts as the one-line mutation it is.
            let diff = similar::TextDiff::configure()
                .timeout(TOOL_DIFF_LINE_BUDGET)
                .diff_lines(before, after);
            diff.ops()
                .iter()
                .fold((0, 0), |(additions, deletions), op| match op.tag() {
                    similar::DiffTag::Equal => (additions, deletions),
                    _ => (
                        additions + u64::try_from(op.new_range().len()).unwrap_or(u64::MAX),
                        deletions + u64::try_from(op.old_range().len()).unwrap_or(u64::MAX),
                    ),
                })
        }
    }
}

fn line_count(body: &str) -> u64 {
    u64::try_from(body.lines().count()).unwrap_or(u64::MAX)
}

fn push_tool_diff(diffs: &mut Vec<FileDiff>, diff: FileDiff, truncated: &mut bool) {
    if diffs.len() < TOOL_DIFF_MAX_FILES {
        diffs.push(diff);
    } else {
        *truncated = true;
    }
}

fn output_with_diffs(text: String, diffs: Vec<FileDiff>, diffs_truncated: bool) -> ToolOutput {
    ToolOutput {
        text,
        metadata: json!({
            "diffs": diffs,
            "diffsTruncated": diffs_truncated,
        }),
    }
}

const TOOL_OUTPUT_MAX_LINES: usize = 2_000;
const TOOL_OUTPUT_MAX_BYTES: usize = 50 * 1_024;

pub(crate) async fn truncate_output(text: String) -> CoreResult<ToolOutput> {
    truncate_output_at(text, &tool_output_dir()).await
}

pub(crate) async fn truncate_output_at(text: String, directory: &Path) -> CoreResult<ToolOutput> {
    let total_lines = text.lines().count();
    if total_lines <= TOOL_OUTPUT_MAX_LINES && text.len() <= TOOL_OUTPUT_MAX_BYTES {
        return Ok(output(text));
    }
    let preview = output_preview(&text);
    tokio::fs::create_dir_all(directory)
        .await
        .map_err(tool_error)?;
    let path = directory.join(format!("tool_{}.txt", uuid::Uuid::now_v7()));
    tokio::fs::write(&path, &text).await.map_err(tool_error)?;
    let removed = text.len().saturating_sub(preview.len());
    let notice = format!(
        "The tool call succeeded but its output was truncated. Full output saved to: {}\n\
         Use grep on that path or read it with offset/limit.\n\n\
         ...{removed} bytes truncated...",
        path.display()
    );
    Ok(ToolOutput {
        text: format!("{preview}\n\n{notice}"),
        metadata: json!({"truncated": true, "outputPath": path}),
    })
}

fn output_preview(text: &str) -> String {
    let mut preview = Vec::new();
    let mut bytes = 0;
    for line in text.lines().take(TOOL_OUTPUT_MAX_LINES) {
        let size = line.len() + usize::from(!preview.is_empty());
        if bytes + size > TOOL_OUTPUT_MAX_BYTES {
            // A first line too long to show whole (minified code, a JSON
            // dump) shows its head, so the preview is never empty.
            if preview.is_empty() {
                return text::shown(line, true).into_owned();
            }
            break;
        }
        preview.push(line);
        bytes += size;
    }
    preview.join("\n")
}

pub(crate) fn tool_output_dir() -> PathBuf {
    crate::data_folder().map_or_else(
        || std::env::temp_dir().join("haarness/tool-output"),
        |folder| folder.join("tool-output"),
    )
}
fn tool_error(error: impl std::fmt::Display) -> CoreError {
    CoreError::Tool(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::State, http::HeaderMap, routing::post};
    use haarness_core::ToolContext;
    use haarness_protocol::{ThreadId, TurnId};
    use haarness_workflow_catalog::WorkflowCatalog;
    use tokio_util::sync::CancellationToken;

    type MediaRequests = Arc<parking_lot::Mutex<Vec<(HeaderMap, Value)>>>;

    struct StubManagement;

    fn language_fixture(name: &str) -> String {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../haarness-language/tests/fixtures")
            .join(name)
            .display()
            .to_string()
    }

    fn language_test_service(workspace: &Path) -> Arc<LanguageService> {
        Arc::new(LanguageService::new(
            workspace.to_path_buf(),
            haarness_language::LspConfig {
                builtins: false,
                diagnostic_timeout_ms: 2_000,
                request_timeout_ms: 2_000,
                servers: std::collections::BTreeMap::from([(
                    "mock".into(),
                    haarness_language::LspServerConfig {
                        command: vec!["python3".into(), language_fixture("mock_lsp.py")],
                        extensions: vec![".mock".into()],
                        ..haarness_language::LspServerConfig::default()
                    },
                )]),
                ..haarness_language::LspConfig::default()
            },
            haarness_language::FormatterConfig {
                enabled: true,
                builtins: false,
                formatters: std::collections::BTreeMap::from([(
                    "mockfmt".into(),
                    haarness_language::FormatterDefinition {
                        command: vec![
                            "python3".into(),
                            language_fixture("mock_formatter.py"),
                            "$FILE".into(),
                        ],
                        extensions: vec![".mock".into()],
                        ..haarness_language::FormatterDefinition::default()
                    },
                )]),
            },
        ))
    }

    fn tool_context(workspace: &Path) -> ToolContext {
        ToolContext {
            execution: haarness_core::ExecutionSettings::default(),
            thread_id: ThreadId::new(),
            turn_id: TurnId::new(),
            workspace: workspace.to_path_buf(),
            authorized_external_paths: Vec::new(),
            cancel: CancellationToken::new(),
            invoker: None,
        }
    }

    #[tokio::test]
    async fn read_attaches_nested_instruction_files_once_per_thread() {
        let root =
            std::env::temp_dir().join(format!("haarness-read-nested-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(root.join("crates/api/src")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "ROOT_RULES").unwrap();
        std::fs::write(root.join("crates/AGENTS.md"), "CRATE_RULES").unwrap();
        std::fs::write(root.join("crates/api/CLAUDE.md"), "CLAUDE_RULES").unwrap();
        std::fs::write(root.join("crates/api/src/lib.rs"), "fn api() {}").unwrap();
        let workspace = root.canonicalize().unwrap();

        let registry = NativeToolRegistry::new(workspace.clone());
        let read = registry.get("read").unwrap();
        let context = tool_context(&workspace);
        let first = read
            .execute(json!({"path":"crates/api/src/lib.rs"}), context.clone())
            .await
            .unwrap();
        assert!(first.text.starts_with("fn api() {}"), "{}", first.text);
        assert!(first.text.contains("CRATE_RULES"));
        assert!(
            !first.text.contains("CLAUDE_RULES"),
            "Claude files are opt-in"
        );
        assert!(
            !first.text.contains("ROOT_RULES"),
            "the root file is already in the system prompt"
        );
        assert_eq!(first.metadata["loaded"].as_array().unwrap().len(), 1);

        let second = read
            .execute(json!({"path":"crates/api/src/lib.rs"}), context.clone())
            .await
            .unwrap();
        assert!(
            !second.text.contains("CRATE_RULES"),
            "attached once per thread"
        );
        assert!(second.metadata.get("loaded").is_none());
        let other_thread = read
            .execute(
                json!({"path":"crates/api/src/lib.rs"}),
                tool_context(&workspace),
            )
            .await
            .unwrap();
        assert!(other_thread.text.contains("CRATE_RULES"));

        let compat = NativeToolRegistry::new(workspace.clone()).with_claude_instructions(true);
        let with_claude = compat
            .get("read")
            .unwrap()
            .execute(
                json!({"path":"crates/api/src/lib.rs"}),
                tool_context(&workspace),
            )
            .await
            .unwrap();
        assert!(with_claude.text.contains("CLAUDE_RULES"));
        assert!(with_claude.text.contains("CRATE_RULES"));
        let loaded = with_claude.metadata["loaded"].as_array().unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(
            loaded[0]
                .as_str()
                .unwrap()
                .ends_with("crates/api/CLAUDE.md"),
            "nearest first"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn read_returns_pdf_and_audio_as_provider_visible_attachments() {
        let root =
            std::env::temp_dir().join(format!("haarness-read-media-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let pdf = root.join("notes.pdf");
        let audio = root.join("meeting.mp3");
        std::fs::write(&pdf, b"%PDF-1.4\n1 0 obj<<>>endobj\n%%EOF\n").unwrap();
        std::fs::write(&audio, b"ID3").unwrap();
        let registry = NativeToolRegistry::new(root.clone());

        let pdf_output = registry
            .get("read")
            .unwrap()
            .execute(json!({"path":"notes.pdf"}), tool_context(&root))
            .await
            .unwrap();
        assert!(pdf_output.text.starts_with("PDF read successfully:"));
        assert_eq!(pdf_output.metadata["mime"], "application/pdf");
        assert_eq!(
            pdf_output.metadata["attachments"][0],
            pdf.canonicalize().unwrap().display().to_string()
        );

        let audio_output = registry
            .get("read")
            .unwrap()
            .execute(json!({"path":"meeting.mp3"}), tool_context(&root))
            .await
            .unwrap();
        assert!(audio_output.text.starts_with("Audio read successfully:"));
        assert_eq!(audio_output.metadata["mime"], "audio/mpeg");
        assert_eq!(
            audio_output.metadata["attachments"][0],
            audio.canonicalize().unwrap().display().to_string()
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn read_shows_images_as_providers_take_them() {
        let root =
            std::env::temp_dir().join(format!("haarness-read-image-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        // A valid 1×1 PNG, the same pixels saved as BMP, and text named .png.
        let png = STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC")
            .unwrap();
        std::fs::write(root.join("dot.png"), &png).unwrap();
        let mut bmp = b"BM".to_vec();
        for value in [58_u32, 0, 54, 40, 1, 1] {
            bmp.extend(value.to_le_bytes());
        }
        bmp.extend([1, 0, 24, 0]);
        bmp.extend([0_u8; 24]);
        bmp.extend([0, 0, 255, 0]);
        std::fs::write(root.join("dot.bmp"), &bmp).unwrap();
        std::fs::write(root.join("notes.png"), b"just text").unwrap();
        let registry = NativeToolRegistry::new(root.clone());
        let read = |path: &'static str| {
            let registry = &registry;
            let root = &root;
            async move {
                registry
                    .get("read")
                    .unwrap()
                    .execute(json!({ "path": path }), tool_context(root))
                    .await
            }
        };

        let kept = read("dot.png").await.unwrap();
        assert_eq!(kept.metadata["mime"], "image/png");
        assert_eq!(
            kept.metadata["attachments"][0],
            root.join("dot.png")
                .canonicalize()
                .unwrap()
                .display()
                .to_string()
        );
        let converted = read("dot.bmp").await.unwrap();
        assert!(
            converted.text.contains("shown to you as a 1×1 PNG"),
            "{}",
            converted.text
        );
        assert_eq!(converted.metadata["mime"], "image/png");
        assert!(
            converted.metadata["attachments"][0]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;name=dot.bmp;base64,iVBORw0KGgo")
        );
        let refused = read("notes.png").await.unwrap_err().to_string();
        assert!(refused.contains("notes.png is not an image"), "{refused}");

        let _ = std::fs::remove_dir_all(root);
    }

    async fn media_fixture(
        State(requests): State<MediaRequests>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let video = body["model"] == "video-fixture";
        requests.lock().push((headers, body));
        if video {
            Json(json!({
                "data":"AAAAIGZ0eXA=",
                "mime":"video/mp4"
            }))
        } else {
            Json(json!({
                "data":[{"b64_json":"iVBORw0KGgo="}],
                "mime_type":"image/png"
            }))
        }
    }

    #[tokio::test]
    async fn optional_generation_backends_write_bounded_provider_visible_files() {
        let requests = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let router = Router::new()
            .route("/generate", post(media_fixture))
            .with_state(requests.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let endpoint = || MediaEndpoint {
            url: format!("http://{address}/generate"),
            api_key: Some("fixture-secret".into()),
        };
        let root = std::env::temp_dir().join(format!("haarness-generate-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let client = reqwest::Client::new();
        let cancel = tokio_util::sync::CancellationToken::new();

        let image = generate_media_from_endpoint(
            GeneratedMediaKind::Image,
            &json!({"prompt":"Quiet red fox", "model":"image-fixture", "size":"1024x1024"}),
            &root,
            &client,
            &cancel,
            endpoint(),
        )
        .await
        .unwrap();
        let image_path = PathBuf::from(image.metadata["path"].as_str().unwrap());
        assert!(image_path.starts_with(&root));
        assert_eq!(
            image_path.extension().and_then(|value| value.to_str()),
            Some("png")
        );
        assert_eq!(std::fs::read(&image_path).unwrap(), b"\x89PNG\r\n\x1a\n");
        assert_eq!(image.metadata["attachments"][0], image.metadata["path"]);

        let video = generate_media_from_endpoint(
            GeneratedMediaKind::Video,
            &json!({"prompt":"Slow orbit", "model":"video-fixture", "aspectRatio":"16:9", "duration":4}),
            &root,
            &client,
            &cancel,
            endpoint(),
        )
        .await
        .unwrap();
        let video_path = PathBuf::from(video.metadata["path"].as_str().unwrap());
        assert_eq!(
            video_path.extension().and_then(|value| value.to_str()),
            Some("mp4")
        );
        assert_eq!(std::fs::read(&video_path).unwrap(), b"\0\0\0 ftyp");

        let recorded = requests.lock();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].1["output_format"], "png");
        assert_eq!(recorded[1].1["aspect_ratio"], "16:9");
        assert_eq!(recorded[1].1["duration"], 4);
        assert_eq!(
            recorded[0].0.get("authorization").unwrap(),
            "Bearer fixture-secret"
        );
        drop(recorded);

        server.abort();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn generated_media_rejects_mime_spoofing() {
        let error =
            validate_generated_media(GeneratedMediaKind::Image, "image/png", b"this is not a png")
                .unwrap_err();
        assert!(error.to_string().contains("do not match"));
        assert!(
            validate_generated_media(
                GeneratedMediaKind::Video,
                "video/webm",
                b"\x1a\x45\xdf\xa3fixture"
            )
            .is_ok()
        );
    }

    #[test]
    fn custom_media_backends_never_inherit_the_openai_key() {
        assert_eq!(
            select_media_api_key(false, None, Some("openai-secret".into())),
            None
        );
        assert_eq!(
            select_media_api_key(
                false,
                Some("dedicated-secret".into()),
                Some("openai-secret".into())
            )
            .as_deref(),
            Some("dedicated-secret")
        );
        assert_eq!(
            select_media_api_key(true, None, Some("openai-secret".into())).as_deref(),
            Some("openai-secret")
        );
    }

    #[tokio::test]
    async fn native_file_tools_format_refresh_diffs_report_diagnostics_and_query_lsp() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-language-tools-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        let language = language_test_service(&workspace);
        let registry = NativeToolRegistry::new(workspace.clone()).with_language(language.clone());

        let write = registry.get("write").unwrap();
        let written = write
            .execute(
                json!({"path":"sample.mock", "content":"BADFMT BROKEN\n"}),
                tool_context(&workspace),
            )
            .await
            .unwrap();
        assert!(
            written.text.contains("Formatted sample.mock with mockfmt"),
            "{}",
            written.text
        );
        assert!(written.text.contains("mock syntax error"));
        assert_eq!(written.metadata["diffs"][0]["after"], "GOODFMT BROKEN\n");

        let read = registry.get("read").unwrap();
        let read = read
            .execute(json!({"path":"sample.mock"}), tool_context(&workspace))
            .await
            .unwrap();
        assert_eq!(read.text, "GOODFMT BROKEN");

        let edit = registry.get("edit").unwrap();
        let edited = edit
            .execute(
                json!({"path":"sample.mock", "old":"BROKEN", "new":"FIXED BADFMT"}),
                tool_context(&workspace),
            )
            .await
            .unwrap();
        assert_eq!(
            edited.metadata["diffs"][0]["after"],
            "GOODFMT FIXED GOODFMT\n"
        );

        let patch = registry.get("apply_patch").unwrap();
        let patched = patch
            .execute(
                json!({"input":"*** Begin Patch\n*** Update File: sample.mock\n@@\n-GOODFMT FIXED GOODFMT\n+BADFMT BROKEN\n*** End Patch"}),
                tool_context(&workspace),
            )
            .await
            .unwrap();
        assert_eq!(patched.metadata["diffs"][0]["after"], "GOODFMT BROKEN\n");
        assert!(patched.text.contains("mock syntax error"));

        let lsp = registry.get("lsp").unwrap();
        let result = lsp
            .execute(
                json!({"operation":"hover", "filePath":"sample.mock", "line":1, "character":1}),
                tool_context(&workspace),
            )
            .await
            .unwrap();
        assert_eq!(result.metadata["result"]["results"][0]["server"], "mock");
        assert!(result.text.contains("mock hover"));

        language.shutdown().await;
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    #[async_trait]
    impl ManagementRegistry for StubManagement {
        async fn execute(
            &self,
            resource: &str,
            arguments: Value,
            _context: ToolContext,
        ) -> CoreResult<ToolOutput> {
            Ok(ToolOutput {
                text: format!("{resource}:{}", arguments["action"]),
                metadata: json!({}),
            })
        }
    }

    #[test]
    fn cloud_tools_are_discoverable_validated_and_honor_read_parallelism() {
        let registry = NativeToolRegistry::new(PathBuf::from("/workspace"))
            .with_management(Arc::new(StubManagement));
        for (name, read, mutation) in [
            ("cloud_agent_manage", "models", "create"),
            ("cloud_workspace", "messages", "ask"),
        ] {
            let tool = registry.get(name).expect("Cloud tool registered");
            assert!(tool.parallel_safe(&json!({"action":read})));
            assert!(!tool.parallel_safe(&json!({"action":mutation})));
            let validator = jsonschema::validator_for(&tool.definition().input_schema).unwrap();
            assert!(validator.is_valid(&json!({"action":read})));
            assert!(!validator.is_valid(&json!({"action":read,"token":"untrusted"})));
            assert!(!validator.is_valid(&json!({"action":"invented"})));
            if name == "cloud_agent_manage" {
                assert!(validator.is_valid(&json!({"action":"create","name":"test","configuration":{"resourceClass":"performance-4cpu-8gb","diskGb":500}})));
                assert!(!validator.is_valid(&json!({"action":"create","name":"test","configuration":{"environment":{"KEY":"secret"}}})));
            } else {
                for action in ["create", "ask"] {
                    assert!(validator.is_valid(&json!({"action":action,"message":"freestyle","configuration":{"instructions":"Review the repository","model":"openai/gpt-5-nano"}})));
                    assert!(validator.is_valid(&json!({"action":action,"message":"use template","cloudAgentId":"00000000-0000-4000-8000-000000000001","configuration":{"instructions":"Override template defaults"}})));
                }
            }
        }
    }

    #[test]
    fn management_tools_expose_only_non_marketplace_actions() {
        let registry: Arc<dyn ManagementRegistry> = Arc::new(StubManagement);
        let expectations = [
            (
                ManagementKind::Skill,
                "skill_manage",
                vec!["create", "edit", "delete", "get", "list", "import"],
            ),
            (
                ManagementKind::Plugin,
                "plugin_manage",
                vec![
                    "create", "edit", "delete", "get", "list", "enable", "disable", "import",
                ],
            ),
            (
                ManagementKind::Mcp,
                "mcp_manage",
                vec![
                    "add", "edit", "remove", "get", "list", "auth", "logout", "enable", "disable",
                ],
            ),
            (
                ManagementKind::Agent,
                "agent_manage",
                vec!["create", "edit", "delete", "get", "list"],
            ),
            (
                ManagementKind::Command,
                "command_manage",
                vec!["create", "edit", "delete", "get", "list"],
            ),
            (ManagementKind::MemoryList, "memory_list", vec![]),
            (ManagementKind::MemoryRead, "memory_read", vec![]),
            (ManagementKind::MemorySearch, "memory_search", vec![]),
            (ManagementKind::MemorySave, "memory_save", vec![]),
            (ManagementKind::MemoryDelete, "memory_delete", vec![]),
            (ManagementKind::TaskAbort, "task_abort", vec!["abort"]),
            (
                ManagementKind::SessionManage,
                "session_manage",
                vec!["list", "search", "read", "digest"],
            ),
            (
                ManagementKind::SessionRename,
                "session_rename",
                vec!["rename"],
            ),
            (
                ManagementKind::SessionMessage,
                "session_message",
                vec!["send"],
            ),
            (ManagementKind::SessionWait, "session_wait", vec!["wait"]),
            (ManagementKind::AgentSwitch, "agent_switch", vec!["switch"]),
        ];
        for (kind, name, actions) in expectations {
            let tool = ManagementTool {
                kind,
                registry: registry.clone(),
            };
            let definition = tool.definition();
            assert_eq!(definition.name, name);
            if matches!(
                kind,
                ManagementKind::Skill
                    | ManagementKind::Plugin
                    | ManagementKind::Mcp
                    | ManagementKind::Agent
                    | ManagementKind::Command
                    | ManagementKind::SessionManage
            ) {
                assert_eq!(
                    definition.input_schema["properties"]["action"]["enum"],
                    json!(actions)
                );
            } else {
                assert!(
                    definition.input_schema["properties"]
                        .get("action")
                        .is_none()
                );
            }
            let encoded = definition.input_schema.to_string();
            for forbidden in [
                "install",
                "uninstall",
                "push",
                "publish",
                "versions",
                "activate",
                "restore",
                "marketplace",
                "dockerHostingEnabled",
            ] {
                assert!(
                    !encoded.contains(forbidden),
                    "{name} leaked {forbidden}: {encoded}"
                );
            }
            assert!(!tool.side_effectful_for(&json!({"action":"list"})));
            assert!(!tool.side_effectful_for(&json!({"action":"get"})));
            let mutation = actions.first().copied().unwrap_or("manage");
            if matches!(
                kind,
                ManagementKind::Skill
                    | ManagementKind::Plugin
                    | ManagementKind::Mcp
                    | ManagementKind::Agent
                    | ManagementKind::Command
            ) {
                assert!(tool.side_effectful_for(&json!({"action":mutation})));
            } else {
                assert!(!tool.side_effectful_for(&json!({"action":mutation})));
            }
        }
    }

    #[tokio::test]
    async fn schedule_tool_manages_disabled_native_jobs_without_a_server_round_trip() {
        let root =
            std::env::temp_dir().join(format!("haarness-schedule-tool-{}", uuid::Uuid::now_v7()));
        let workspace = root.join("workspace");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        let tool = ScheduleTool {
            workspace: workspace.clone(),
            scheduler: Scheduler::with_paths(
                haarness_scheduler::SchedulerPaths {
                    config: root.join("config"),
                    state: root.join("state"),
                    launchd: root.join("launchd"),
                    systemd: root.join("systemd"),
                    windows: root.join("windows"),
                },
                PathBuf::from("/opt/bin/haarness"),
                None,
                haarness_scheduler::Backend::Launchd,
                false,
            ),
            trust_project: true,
        };
        assert!(
            !tool.side_effectful(),
            "external scheduler state must not create workspace snapshots"
        );
        let context = ToolContext {
            execution: haarness_core::ExecutionSettings::default(),
            thread_id: ThreadId::new(),
            turn_id: TurnId::new(),
            workspace: workspace.clone(),
            authorized_external_paths: Vec::new(),
            cancel: CancellationToken::new(),
            invoker: None,
        };
        let created = tool
            .execute(
                json!({
                    "action":"create",
                    "name":"daily",
                    "prompt":"summarize",
                    "enabled":false,
                    "trigger":{"type":"cron","expression":"0 8 * * *"}
                }),
                context.clone(),
            )
            .await
            .unwrap();
        assert_eq!(created.metadata["schedule"]["trustProject"], true);
        assert_eq!(created.metadata["schedule"]["enabled"], false);
        let listed = tool
            .execute(json!({"action":"list"}), context.clone())
            .await
            .unwrap();
        assert!(listed.text.contains("daily"), "{}", listed.text);
        let updated = tool
            .execute(
                json!({"action":"update","name":"daily","prompt":"summarize changes"}),
                context.clone(),
            )
            .await
            .unwrap();
        assert!(
            updated.text.contains("summarize changes"),
            "{}",
            updated.text
        );
        tool.execute(json!({"action":"delete","name":"daily"}), context)
            .await
            .unwrap();
        assert!(tool.scheduler.list().unwrap().is_empty());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn a_schedule_made_in_a_chat_folder_runs_there() {
        let root =
            std::env::temp_dir().join(format!("haarness-schedule-folder-{}", uuid::Uuid::now_v7()));
        let workspace = root.join("home");
        let folder = workspace.join("2026-10-24/chat-ab12");
        tokio::fs::create_dir_all(&folder).await.unwrap();
        let tool = ScheduleTool {
            workspace: workspace.clone(),
            scheduler: Scheduler::with_paths(
                haarness_scheduler::SchedulerPaths {
                    config: root.join("config"),
                    state: root.join("state"),
                    launchd: root.join("launchd"),
                    systemd: root.join("systemd"),
                    windows: root.join("windows"),
                },
                PathBuf::from("/opt/bin/haarness"),
                None,
                haarness_scheduler::Backend::Launchd,
                false,
            ),
            trust_project: false,
        };
        let context = |workspace: &Path| ToolContext {
            execution: haarness_core::ExecutionSettings::default(),
            thread_id: ThreadId::new(),
            turn_id: TurnId::new(),
            workspace: workspace.to_path_buf(),
            authorized_external_paths: Vec::new(),
            cancel: CancellationToken::new(),
            invoker: None,
        };
        let create = |name: &str| {
            json!({
                "action": "create",
                "name": name,
                "prompt": "summarize",
                "enabled": false,
                "trigger": {"type": "cron", "expression": "0 8 * * *"}
            })
        };
        let canonical = folder.canonicalize().unwrap();
        let in_folder = tool
            .execute(create("from-chat"), context(&folder))
            .await
            .unwrap();
        assert_eq!(
            in_folder.metadata["schedule"]["directory"],
            canonical.display().to_string()
        );
        assert_eq!(
            in_folder.metadata["schedule"]["workspace"],
            workspace.canonicalize().unwrap().display().to_string()
        );
        let in_project = tool
            .execute(create("from-project"), context(&workspace))
            .await
            .unwrap();
        assert!(in_project.metadata["schedule"].get("directory").is_none());
        // A chat folder schedules only into itself: any workspace is refused.
        for named in [root.join("home"), folder.clone()] {
            let mut named_create = create("elsewhere");
            named_create["workspace"] = json!(named.display().to_string());
            let refused = tool
                .execute(named_create, context(&folder))
                .await
                .unwrap_err();
            assert!(
                matches!(&refused, CoreError::Invalid(message) if message.contains("own folder")),
                "{refused}"
            );
        }
        // Another chat's folder neither sees nor touches it.
        let other = workspace.join("2026-10-24/chat-cd34");
        tokio::fs::create_dir_all(&other).await.unwrap();
        let listed = tool
            .execute(json!({"action": "list"}), context(&other))
            .await
            .unwrap();
        assert_eq!(listed.metadata["schedule"], json!([]), "{}", listed.text);
        for action in [
            json!({"action": "history", "name": "from-chat"}),
            json!({"action": "run", "name": "from-chat"}),
            json!({"action": "delete", "name": "from-chat"}),
            json!({"action": "enable", "name": "from-chat"}),
            json!({"action": "disable", "name": "from-chat"}),
            json!({"action": "update", "name": "from-chat", "prompt": "changed"}),
            json!({"action": "update", "name": "from-project", "prompt": "changed"}),
        ] {
            let refused = tool
                .execute(action.clone(), context(&other))
                .await
                .unwrap_err();
            assert!(
                refused.to_string().contains("schedule not found"),
                "{action}: {refused}"
            );
        }
        let mut moved = json!({"action": "update", "name": "from-chat", "prompt": "x"});
        moved["workspace"] = json!(workspace.display().to_string());
        assert!(tool.execute(moved, context(&folder)).await.is_err());
        // Its own chat sees and changes only its own.
        let own = tool
            .execute(json!({"action": "list"}), context(&folder))
            .await
            .unwrap();
        let names = own.metadata["schedule"]
            .as_array()
            .unwrap()
            .iter()
            .map(|schedule| schedule["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, ["from-chat"]);
        let updated = tool
            .execute(
                json!({"action": "update", "name": "from-chat", "prompt": "summarize again"}),
                context(&folder),
            )
            .await
            .unwrap();
        assert_eq!(updated.metadata["schedule"]["prompt"], "summarize again");
        tool.execute(
            json!({"action": "disable", "name": "from-chat"}),
            context(&folder),
        )
        .await
        .unwrap();
        tool.execute(
            json!({"action": "history", "name": "from-chat"}),
            context(&folder),
        )
        .await
        .unwrap();
        // A chat without a folder of its own sees only schedules without one,
        // and can act on no chat's.
        let projects = tool
            .execute(json!({"action": "list"}), context(&workspace))
            .await
            .unwrap();
        let names = projects.metadata["schedule"]
            .as_array()
            .unwrap()
            .iter()
            .map(|schedule| schedule["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, ["from-project"]);
        for action in [
            json!({"action": "history", "name": "from-chat"}),
            json!({"action": "run", "name": "from-chat"}),
            json!({"action": "delete", "name": "from-chat"}),
            json!({"action": "enable", "name": "from-chat"}),
            json!({"action": "disable", "name": "from-chat"}),
            json!({"action": "update", "name": "from-chat", "prompt": "changed"}),
        ] {
            let refused = tool
                .execute(action.clone(), context(&workspace))
                .await
                .unwrap_err();
            assert!(
                refused.to_string().contains("schedule not found"),
                "{action}: {refused}"
            );
        }
        tool.execute(
            json!({"action": "update", "name": "from-project", "prompt": "still mine"}),
            context(&workspace),
        )
        .await
        .unwrap();
        tool.execute(
            json!({"action": "delete", "name": "from-chat"}),
            context(&folder),
        )
        .await
        .unwrap();
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[test]
    fn workflow_tool_rejects_fabricated_wrong_typed_options() {
        for (input, expected) in [
            (json!({"action": 1}), "action"),
            (json!({"action":"read", "name": false}), "name"),
            (json!({"action":"start", "model": false}), "model"),
            (json!({"action":"wait", "timeout": 0}), "timeout"),
            (
                json!({"action":"start", "timeout": 5_000}),
                "executionTimeout",
            ),
            (
                json!({"action":"wait", "executionTimeout": 5_000}),
                "only valid",
            ),
            (json!({"action":"create", "overwrite":"yes"}), "overwrite"),
        ] {
            let error = validate_workflow_tool_input(input.as_object().unwrap()).unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[test]
    fn workflow_model_summary_omits_source_and_agent_transcript_payloads() {
        let run: WorkflowRun = serde_json::from_value(json!({
            "id": WorkflowId::new(),
            "threadId": ThreadId::new(),
            "directory": "/private/workspace",
            "workflowName": "audit",
            "sourceHash": "secret-source-hash",
            "status": "running",
            "logs": [{
                "sequence": 1,
                "time": "2026-08-24T00:00:00Z",
                "message": "started",
                "source": "platform"
            }],
            "agents": [{
                "id": "agent-1",
                "label": "inspect auth",
                "status": "running",
                "threadId": ThreadId::new(),
                "output": "large child transcript that must not be repeated",
                "startedAt": "2026-08-24T00:00:00Z"
            }],
            "definition": {
                "name": "audit",
                "invocation": "audit",
                "dialect": "claude-portable",
                "meta": {"name": "audit"},
                "source": "return await agent('inspect')"
            },
            "time": {
                "created": "2026-08-24T00:00:00Z",
                "updated": "2026-08-24T00:00:01Z",
                "started": "2026-08-24T00:00:00Z"
            }
        }))
        .expect("workflow fixture");
        let summary = workflow_run_summary(&run);
        let text = serde_json::to_string(&summary).expect("summary serializes");
        assert_eq!(summary["runID"], json!(run.id));
        assert_eq!(summary["agentCounts"]["running"], 1);
        assert!(!text.contains("secret-source-hash"), "{text}");
        assert!(!text.contains("private/workspace"), "{text}");
        assert!(!text.contains("large child transcript"), "{text}");
        assert!(!text.contains("return await agent"), "{text}");
    }

    #[tokio::test]
    async fn workflow_create_is_slug_confined_and_never_overwrites() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-workflow-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        let catalog = WorkflowCatalog::for_workspace(&workspace);
        let source = r#"
export default workflow({
  name: "audit",
  async run(_args, ctx) { ctx.log("ok") },
})
"#;
        let created = create_workflow_file(&workspace, &catalog, "safe-audit", source)
            .await
            .unwrap();
        let path = PathBuf::from(created["path"].as_str().unwrap());
        assert_eq!(
            path,
            workspace
                .canonicalize()
                .unwrap()
                .join(".haarness/workflows/safe-audit.ts")
        );
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), source);

        let changed = source.replace("ctx.log(\"ok\")", "ctx.log(\"changed\")");
        let duplicate = create_workflow_file(&workspace, &catalog, "safe-audit", &changed)
            .await
            .unwrap_err();
        assert!(matches!(duplicate, CoreError::Conflict(_)));
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), source);

        for unsafe_name in ["../escape", "nested/name", ".hidden", "Uppercase"] {
            let error = create_workflow_file(&workspace, &catalog, unsafe_name, source)
                .await
                .unwrap_err();
            assert!(
                matches!(error, CoreError::Invalid(_)),
                "{unsafe_name}: {error}"
            );
        }
        assert!(!workspace.parent().unwrap().join("escape.ts").exists());
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    #[tokio::test]
    async fn bash_workdir_is_workspace_confined_and_reports_exit_metadata() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-bash-cwd-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(workspace.join("nested"))
            .await
            .unwrap();
        let registry = NativeToolRegistry::new(workspace.clone());
        let context = ToolContext {
            execution: haarness_core::ExecutionSettings::default(),
            thread_id: ThreadId::new(),
            turn_id: TurnId::new(),
            workspace: workspace.clone(),
            authorized_external_paths: Vec::new(),
            cancel: CancellationToken::new(),
            invoker: None,
        };
        let result = registry
            .get("bash")
            .unwrap()
            .execute(
                json!({"command":"pwd; exit 7", "timeout":10_000, "workdir":"nested"}),
                context.clone(),
            )
            .await
            .unwrap();
        assert_eq!(result.metadata["exit"], 7);
        assert!(result.text.contains("nested"), "{}", result.text);
        let escaped = registry
            .get("bash")
            .unwrap()
            .execute(json!({"command":"pwd", "workdir":"../"}), context)
            .await;
        assert!(matches!(escaped, Err(CoreError::PermissionDenied(_))));
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    #[test]
    fn exposes_all_core_tool_names() {
        let registry = NativeToolRegistry::new(std::env::current_dir().unwrap());
        let names = registry
            .list()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        for expected in [
            "read",
            "list",
            "glob",
            "grep",
            "write",
            "edit",
            "apply_patch",
            "bash",
            "webfetch",
            "question",
            "todo",
            "todowrite",
            "todoread",
            "skill",
            "task",
            "get_goal",
            "update_goal",
            "propose_goal_criteria",
        ] {
            assert!(names.contains(&expected.to_owned()));
        }
    }

    #[test]
    fn bash_definition_uses_the_haarness_timeout_contract() {
        let registry = NativeToolRegistry::new(std::env::current_dir().unwrap());
        let bash = registry.get("bash").unwrap().definition();
        let properties = bash.input_schema["properties"].as_object().unwrap();
        assert!(properties.contains_key("timeout"));
        assert!(!properties.contains_key("timeout_ms"));
        assert!(bash.description.contains("normally 120000"));
        assert_eq!(properties["timeout"]["minimum"], 0);
        assert!(properties["timeout"].get("maximum").is_none());
    }

    #[test]
    fn question_definition_advertises_free_form_answers_by_default() {
        let registry = NativeToolRegistry::new(std::env::current_dir().unwrap());
        let question = registry.get("question").unwrap().definition();
        assert_eq!(
            question.input_schema["properties"]["custom"]["default"],
            true
        );
        assert!(question.description.contains("enabled by default"));
    }

    #[test]
    fn shared_tool_definitions_explain_haarness_style_selection_and_safety_rules() {
        let registry = NativeToolRegistry::new(std::env::current_dir().unwrap());
        let definition = |name: &str| registry.get(name).unwrap().definition();

        let read = definition("read");
        assert!(read.description.contains("already know its path"));
        assert!(read.description.contains("Use grep"));
        assert_eq!(
            read.input_schema["properties"]["offset"]["description"],
            "Zero-based line offset. Defaults to 0."
        );

        let list = definition("list");
        assert!(list.description.contains("not recursive"));
        assert!(list.description.contains("Use glob"));

        let glob = definition("glob");
        assert!(glob.description.contains("filename discovery"));
        assert!(glob.description.contains("reserve task"));
        assert!(
            glob.input_schema["properties"]["path"]["description"]
                .as_str()
                .unwrap()
                .contains("Omit")
        );

        let grep = definition("grep");
        assert!(grep.description.contains("standard ignore files"));
        assert!(grep.description.contains("match counts"));
        assert!(grep.description.contains("reserve task"));

        let write = definition("write");
        assert!(write.description.contains("Read an existing file first"));
        assert!(write.description.contains("Prefer edit or apply_patch"));
        assert!(write.description.contains("unrelated user changes"));

        let edit = definition("edit");
        assert!(edit.description.contains("exactly one occurrence"));
        assert!(edit.description.contains("enough surrounding context"));
        assert!(edit.description.contains("Use apply_patch"));

        let patch = definition("apply_patch");
        assert!(patch.description.contains("explicit *** Add File"));
        assert!(patch.description.contains("Prefix every line"));
        assert!(patch.description.contains("unique locator"));

        let bash = definition("bash");
        assert!(bash.description.contains("Do not use it for ordinary file"));
        assert!(bash.description.contains("Use workdir instead"));
        assert!(bash.description.contains("set 0 for no deadline"));
        assert!(bash.description.contains("commits or push"));

        let webfetch = definition("webfetch");
        assert!(webfetch.description.contains("read-only"));
        assert!(webfetch.description.contains("targeted connected tool"));
        assert_eq!(
            webfetch.input_schema["properties"]["format"]["default"],
            "markdown"
        );

        let question = definition("question");
        assert!(question.description.contains("one blocking question"));
        assert!(question.description.contains("(Recommended)"));
        assert!(question.description.contains("safe, scoped assumption"));

        let todo = definition("todo");
        assert!(todo.description.contains("Legacy alias of todowrite"));
        let todowrite = definition("todowrite");
        assert!(todowrite.description.contains("non-trivial multi-step"));
        assert!(todowrite.description.contains("at most one in_progress"));
        let todoread = definition("todoread");
        assert!(
            todoread
                .description
                .contains("resuming prior multi-step work")
        );

        let skill = definition("skill");
        assert!(skill.description.contains("task clearly matches"));
        assert!(skill.description.contains("Do not invent a name"));

        let get_goal = definition("get_goal");
        assert!(get_goal.description.contains("before deciding"));
        let criteria = definition("propose_goal_criteria");
        assert!(criteria.description.contains("before substantive work"));
        assert!(criteria.description.contains("without compound and/or"));
    }

    #[test]
    fn task_definition_tells_models_to_omit_new_child_ids() {
        let registry = NativeToolRegistry::new(std::env::current_dir().unwrap());
        let task = registry.get("task").unwrap().definition();
        let task_id = &task.input_schema["properties"]["task_id"];
        assert_eq!(task_id["minLength"], 1);
        assert!(task_id["description"].as_str().unwrap().contains("Omit"));
        assert!(
            task.input_schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .all(|field| field != "task_id")
        );
    }

    #[test]
    fn task_definition_makes_foreground_the_dependency_based_default() {
        let registry = NativeToolRegistry::new(std::env::current_dir().unwrap());
        let task = registry.get("task").unwrap().definition();
        let background = &task.input_schema["properties"]["background"];
        let description = background["description"].as_str().unwrap();

        assert_eq!(background["default"], false);
        assert!(
            task.description
                .contains("based on dependency, not task size")
        );
        assert!(task.description.contains("Foreground is the default"));
        assert!(description.contains("Defaults to false"));
        assert!(description.contains("non-overlapping"));
        assert!(description.contains("immediate next step"));
        assert!(description.contains("final report"));
        assert!(
            task.description
                .contains("already start concurrently, including foreground tasks")
        );
        assert!(task.description.contains("For a broad audit"));
        assert!(task.description.contains("mutually exclusive"));
        assert!(task.description.contains("auditing a sibling's scope"));
        assert!(task.description.contains("Do not launch background audits"));
        assert!(task.description.contains("synthesize directly"));
        assert!(
            task.description
                .contains("do not re-read every delegated file")
        );
        assert!(task.description.contains("Cross-cutting tests"));
        assert!(description.contains("already run concurrently with background=false"));
        assert!(description.contains("different files and topics"));
        assert!(description.contains("launch distinct foreground task calls together"));
        assert!(description.contains("Do not routinely re-read delegated files"));
        assert!(
            task.input_schema["properties"]["prompt"]["description"]
                .as_str()
                .unwrap()
                .contains("exclusive file/topic boundary")
        );
    }

    #[test]
    fn update_goal_definition_requires_completion_question_conditionally() {
        let registry = NativeToolRegistry::new(std::env::current_dir().unwrap());
        let update = registry.get("update_goal").unwrap().definition();
        assert!(
            update
                .description
                .contains("Completion requires next_step_question")
        );
        assert!(
            update.input_schema["properties"]["next_step_question"]["description"]
                .as_str()
                .unwrap()
                .contains("must offer Edit, New, Clear, and Dismiss")
        );
        assert!(
            update.input_schema["properties"]["status"]["description"]
                .as_str()
                .unwrap()
                .contains("Completing requires next_step_question")
        );
        assert_eq!(
            update.input_schema["properties"]["next_step_question"]["properties"]["edit"]["properties"]
                ["label"]["minLength"],
            1
        );
        assert_eq!(
            update.input_schema["required"],
            json!(["status"]),
            "blocked updates must not require a completion prompt"
        );
    }

    #[tokio::test]
    async fn skill_tool_loads_skill_md_body() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-skill-{}", uuid::Uuid::now_v7()));
        let skills_root = workspace.join("skills-root");
        tokio::fs::create_dir_all(skills_root.join("demo"))
            .await
            .unwrap();
        tokio::fs::write(
            skills_root.join("demo/SKILL.md"),
            "---\ndescription: Demo skill\n---\nDo the demo thing.\n",
        )
        .await
        .unwrap();
        let registry = crate::FilesystemSkillRegistry::new(vec![(skills_root, true)]);
        let infos = registry.discover().await.unwrap();
        let tools = NativeToolRegistry::with_extras(
            workspace.clone(),
            None,
            Some(Arc::new(registry)),
            infos,
        );
        let skill = tools.get("skill").unwrap();
        assert!(
            skill.definition().description.contains("demo"),
            "catalog should name discovered skills: {}",
            skill.definition().description
        );
        let context = ToolContext {
            execution: haarness_core::ExecutionSettings::default(),
            thread_id: ThreadId::new(),
            turn_id: TurnId::new(),
            workspace: workspace.clone(),
            authorized_external_paths: Vec::new(),
            cancel: CancellationToken::new(),
            invoker: None,
        };
        let loaded = skill
            .execute(json!({"name":"demo"}), context.clone())
            .await
            .unwrap();
        assert!(loaded.text.contains("Do the demo thing."));

        let missing = skill.execute(json!({"name":"absent"}), context).await;
        assert!(missing.is_err());
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    #[tokio::test]
    async fn skill_tool_refuses_untrusted_project_skills() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-skill-untrusted-{}", uuid::Uuid::now_v7()));
        let skills_root = workspace.join(".haarness/skills");
        tokio::fs::create_dir_all(skills_root.join("secret"))
            .await
            .unwrap();
        tokio::fs::write(
            skills_root.join("secret/SKILL.md"),
            "---\ndescription: Untrusted\n---\nBody\n",
        )
        .await
        .unwrap();
        let registry = crate::FilesystemSkillRegistry::new(vec![(skills_root, false)]);
        let infos = registry.discover().await.unwrap();
        let tools = NativeToolRegistry::with_extras(
            workspace.clone(),
            None,
            Some(Arc::new(registry)),
            infos,
        );
        let context = ToolContext {
            execution: haarness_core::ExecutionSettings::default(),
            thread_id: ThreadId::new(),
            turn_id: TurnId::new(),
            workspace: workspace.clone(),
            authorized_external_paths: Vec::new(),
            cancel: CancellationToken::new(),
            invoker: None,
        };
        let refused = tools
            .get("skill")
            .unwrap()
            .execute(json!({"name":"secret"}), context)
            .await;
        assert!(matches!(refused, Err(CoreError::PermissionDenied(_))));
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    #[tokio::test]
    async fn apply_patch_refuses_a_move_onto_the_same_file() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-patch-self-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::write(workspace.join("notes.txt"), "one\n")
            .await
            .unwrap();
        tokio::fs::write(workspace.join("first.txt"), "first\n")
            .await
            .unwrap();
        let registry = NativeToolRegistry::new(workspace.clone());
        let run = |input: String| {
            let registry = &registry;
            let context = tool_context(&workspace);
            async move {
                registry
                    .get("apply_patch")
                    .unwrap()
                    .execute(json!({ "input": input }), context)
                    .await
            }
        };
        let mut targets = vec!["notes.txt".to_owned(), "./notes.txt".to_owned()];
        // On a case-insensitive disk another case is the same file too.
        if workspace.join("NOTES.TXT").exists() {
            targets.push("NOTES.TXT".to_owned());
        }
        for target in targets {
            let input = format!(
                "*** Begin Patch\n*** Update File: first.txt\n@@\n-first\n+changed\n\
                 *** Update File: notes.txt\n*** Move to: {target}\n@@\n-one\n+two\n*** End Patch\n"
            );
            let error = run(input).await.unwrap_err().to_string();
            assert!(error.contains("names the same file"), "{target}: {error}");
            assert_eq!(
                std::fs::read_to_string(workspace.join("notes.txt")).unwrap(),
                "one\n"
            );
            // Refused before anything was written.
            assert_eq!(
                std::fs::read_to_string(workspace.join("first.txt")).unwrap(),
                "first\n"
            );
        }
        // A file the same patch added, moved onto itself.
        let error = run(
            "*** Begin Patch\n*** Add File: added.txt\n+a\n*** Update File: added.txt\n*** Move to: ./added.txt\n@@\n-a\n+b\n*** End Patch\n"
                .to_owned(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("names the same file"), "{error}");
        // Refused before the file it adds was written.
        assert!(!workspace.join("added.txt").exists());
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    #[tokio::test]
    async fn apply_patch_plans_every_operation_before_it_writes() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-patch-plan-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        std::fs::write(workspace.join("first.txt"), "one\n").unwrap();
        std::fs::write(workspace.join("second.txt"), "two\n").unwrap();
        std::fs::write(workspace.join("blocker"), "a file\n").unwrap();
        std::fs::write(workspace.join("windows.txt"), "a\r\nb\r\nc\r\n").unwrap();
        let registry = NativeToolRegistry::new(workspace.clone());
        let run = |input: &str| {
            let registry = &registry;
            let context = tool_context(&workspace);
            let input = input.to_owned();
            async move {
                registry
                    .get("apply_patch")
                    .unwrap()
                    .execute(json!({ "input": input }), context)
                    .await
            }
        };

        // A later hunk that does not match leaves the earlier file as it was.
        let error = run(concat!(
            "*** Begin Patch\n*** Update File: first.txt\n@@\n-one\n+ONE\n",
            "*** Update File: second.txt\n@@\n-absent\n+x\n*** End Patch\n"
        ))
        .await
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("second.txt: hunk context not found: \"absent\"; no file was changed"),
            "{error}"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("first.txt")).unwrap(),
            "one\n"
        );

        // A write that fails partway says what it already applied.
        let error = run(concat!(
            "*** Begin Patch\n*** Add File: added.txt\n+a\n",
            "*** Add File: blocker/inner.txt\n+b\n*** End Patch\n"
        ))
        .await
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("could not write blocker/inner.txt"),
            "{error}"
        );
        assert!(
            error.ends_with("; already applied: added added.txt"),
            "{error}"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("added.txt")).unwrap(),
            "a\n"
        );

        // Operations read a file as the earlier ones leave it.
        let output = run(concat!(
            "*** Begin Patch\n*** Add File: made.txt\n+one\n",
            "*** Update File: made.txt\n@@\n-one\n+two\n",
            "*** Delete File: first.txt\n*** Add File: first.txt\n+fresh\n",
            "*** Update File: windows.txt\n@@\n a\n-b\n+B\n+added\n*** End Patch\n"
        ))
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(workspace.join("made.txt")).unwrap(),
            "two\n"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("first.txt")).unwrap(),
            "fresh\n"
        );
        // A CRLF file stays CRLF.
        assert_eq!(
            std::fs::read_to_string(workspace.join("windows.txt")).unwrap(),
            "a\r\nB\r\nadded\r\nc\r\n"
        );
        let diffs: Vec<FileDiff> =
            serde_json::from_value(output.metadata["diffs"].clone()).unwrap();
        let shown = diffs
            .iter()
            .map(|diff| (diff.file.as_str(), diff.status, diff.before.as_deref()))
            .collect::<Vec<_>>();
        assert_eq!(
            shown[..4],
            [
                ("made.txt", FileDiffStatus::Added, None),
                ("made.txt", FileDiffStatus::Modified, Some("one\n")),
                ("first.txt", FileDiffStatus::Deleted, Some("one\n")),
                ("first.txt", FileDiffStatus::Added, None),
            ]
        );
        let error = run(concat!(
            "*** Begin Patch\n*** Delete File: made.txt\n",
            "*** Update File: made.txt\n@@\n-two\n+three\n*** End Patch\n"
        ))
        .await
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("made.txt: an earlier operation removes it; no file was changed"),
            "{error}"
        );
        assert!(workspace.join("made.txt").exists());
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn apply_patch_deletes_and_moves_a_link_itself_not_the_file_it_leads_to() {
        use std::os::unix::fs::symlink;
        let root =
            std::env::temp_dir().join(format!("haarness-patch-links-{}", uuid::Uuid::now_v7()));
        let workspace = root.join("workspace");
        let outside = root.join("outside");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(workspace.join("real.txt"), "real\n").unwrap();
        std::fs::write(outside.join("kept.txt"), "outside\n").unwrap();
        symlink("real.txt", workspace.join("alias.txt")).unwrap();
        symlink(outside.join("kept.txt"), workspace.join("out.txt")).unwrap();
        symlink("missing.txt", workspace.join("dangling.txt")).unwrap();
        let registry = NativeToolRegistry::new(workspace.clone());
        let run = |input: &str| {
            let registry = &registry;
            let context = tool_context(&workspace);
            let input = input.to_owned();
            async move {
                registry
                    .get("apply_patch")
                    .unwrap()
                    .execute(json!({ "input": input }), context)
                    .await
            }
        };

        // Deleting a link removes the link, and the file it leads to stays.
        let deleted = run("*** Begin Patch\n*** Delete File: alias.txt\n*** End Patch\n").await;
        assert!(std::fs::symlink_metadata(workspace.join("alias.txt")).is_err());
        assert_eq!(
            std::fs::read_to_string(workspace.join("real.txt")).unwrap(),
            "real\n"
        );
        // The removed link's diff shows what Git shows for a link.
        let deleted = deleted.unwrap();
        let diffs = deleted.metadata["diffs"].as_array().unwrap();
        assert_eq!(diffs[0]["file"], "alias.txt");
        assert_eq!(diffs[0]["before"], "real.txt");
        // So does one outside the project (no outside folder was granted),
        // and a dangling link goes too.
        run("*** Begin Patch\n*** Delete File: out.txt\n*** Delete File: dangling.txt\n*** End Patch\n")
            .await
            .unwrap();
        for link in ["out.txt", "dangling.txt"] {
            assert!(
                std::fs::symlink_metadata(workspace.join(link)).is_err(),
                "{link}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(outside.join("kept.txt")).unwrap(),
            "outside\n"
        );

        // A move reads through the link and takes away only the link, also
        // when it moves onto the file the link leads to.
        symlink("real.txt", workspace.join("alias.txt")).unwrap();
        run("*** Begin Patch\n*** Update File: alias.txt\n*** Move to: moved.txt\n@@\n-real\n+moved\n*** End Patch\n")
            .await
            .unwrap();
        assert!(std::fs::symlink_metadata(workspace.join("alias.txt")).is_err());
        assert_eq!(
            std::fs::read_to_string(workspace.join("moved.txt")).unwrap(),
            "moved\n"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("real.txt")).unwrap(),
            "real\n"
        );
        symlink("real.txt", workspace.join("alias.txt")).unwrap();
        run("*** Begin Patch\n*** Update File: alias.txt\n*** Move to: real.txt\n@@\n-real\n+through\n*** End Patch\n")
            .await
            .unwrap();
        assert!(std::fs::symlink_metadata(workspace.join("alias.txt")).is_err());
        assert_eq!(
            std::fs::read_to_string(workspace.join("real.txt")).unwrap(),
            "through\n"
        );
        // A move onto the link's own name would write the file it leads to
        // and then remove the link, so it is refused as before.
        symlink("real.txt", workspace.join("alias.txt")).unwrap();
        let mut targets = vec!["alias.txt", "./alias.txt"];
        // On a case-insensitive disk another case is the same entry too.
        if workspace.join("ALIAS.TXT").exists() {
            targets.push("ALIAS.TXT");
        }
        for target in targets {
            let error = run(&format!(
                "*** Begin Patch\n*** Update File: alias.txt\n*** Move to: {target}\n@@\n-through\n+lost\n*** End Patch\n"
            ))
            .await
            .unwrap_err()
            .to_string();
            assert!(error.contains("names the same file"), "{target}: {error}");
            assert!(std::fs::symlink_metadata(workspace.join("alias.txt")).is_ok());
            assert_eq!(
                std::fs::read_to_string(workspace.join("real.txt")).unwrap(),
                "through\n"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn apply_patch_executes_v4a_envelopes_across_files() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-patch-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::write(workspace.join("keep.rs"), "fn one() {}\nfn two() {}\n")
            .await
            .unwrap();
        tokio::fs::write(workspace.join("old.txt"), "obsolete\n")
            .await
            .unwrap();
        let registry = NativeToolRegistry::new(workspace.clone());
        let context = ToolContext {
            execution: haarness_core::ExecutionSettings::default(),
            thread_id: ThreadId::new(),
            turn_id: TurnId::new(),
            workspace: workspace.clone(),
            authorized_external_paths: Vec::new(),
            cancel: CancellationToken::new(),
            invoker: None,
        };
        let patch = "*** Begin Patch\n\
*** Add File: docs/new.md\n\
+created\n\
*** Update File: keep.rs\n\
-fn two() {}\n\
+fn two() { updated(); }\n\
*** Delete File: old.txt\n\
*** End Patch\n";
        let result = registry
            .get("apply_patch")
            .unwrap()
            .execute(json!({"input": patch}), context.clone())
            .await
            .unwrap();
        assert!(result.text.contains("added docs/new.md"), "{}", result.text);
        assert!(result.text.contains("updated keep.rs"));
        assert!(result.text.contains("deleted old.txt"));
        let diffs: Vec<FileDiff> =
            serde_json::from_value(result.metadata["diffs"].clone()).unwrap();
        assert_eq!(diffs.len(), 3, "{diffs:?}");
        assert_eq!(diffs[0].file, "docs/new.md");
        assert_eq!(diffs[0].status, FileDiffStatus::Added);
        assert_eq!(diffs[0].before, None);
        assert_eq!(diffs[0].after.as_deref(), Some("created\n"));
        assert_eq!(diffs[1].file, "keep.rs");
        assert_eq!(diffs[1].status, FileDiffStatus::Modified);
        assert_eq!(
            diffs[1].before.as_deref(),
            Some("fn one() {}\nfn two() {}\n")
        );
        assert_eq!(
            diffs[1].after.as_deref(),
            Some("fn one() {}\nfn two() { updated(); }\n")
        );
        assert_eq!(diffs[2].file, "old.txt");
        assert_eq!(diffs[2].status, FileDiffStatus::Deleted);
        assert_eq!(diffs[2].before.as_deref(), Some("obsolete\n"));
        assert_eq!(diffs[2].after, None);
        assert_eq!(result.metadata["diffsTruncated"], false);
        assert_eq!(
            tokio::fs::read_to_string(workspace.join("keep.rs"))
                .await
                .unwrap(),
            "fn one() {}\nfn two() { updated(); }\n"
        );
        assert!(workspace.join("docs/new.md").exists());
        assert!(!workspace.join("old.txt").exists());

        let escape = registry
            .get("apply_patch")
            .unwrap()
            .execute(
                json!({"input": "*** Begin Patch\n*** Add File: ../escape.txt\n+x\n*** End Patch\n"}),
                context,
            )
            .await;
        assert!(matches!(escape, Err(CoreError::PermissionDenied(_))));
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    #[tokio::test]
    async fn apply_patch_renames_by_letter_case_and_finds_a_file_it_adds_in_another_case() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-patch-case-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        std::fs::write(workspace.join("README.md"), "# hi\n").unwrap();
        let registry = NativeToolRegistry::new(workspace.clone());
        let run = |input: &str| {
            let registry = &registry;
            let context = tool_context(&workspace);
            let input = input.to_owned();
            async move {
                registry
                    .get("apply_patch")
                    .unwrap()
                    .execute(json!({ "input": input }), context)
                    .await
            }
        };
        let names = || {
            let mut names = std::fs::read_dir(&workspace)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect::<Vec<_>>();
            names.sort();
            names
        };
        // On any disk the file ends up as the patch spells it.
        run("*** Begin Patch\n*** Delete File: README.md\n*** Add File: readme.md\n+# hi\n*** End Patch\n")
            .await
            .unwrap();
        assert_eq!(names(), ["readme.md"]);
        // On a disk that ignores letter case, a file the patch adds is found
        // by another case, as it was when each operation wrote at once.
        if workspace.join("README.MD").exists() {
            // In an empty folder too, which asks the folder holding it.
            std::fs::create_dir(workspace.join("empty")).unwrap();
            run(concat!(
                "*** Begin Patch\n*** Add File: Foo.txt\n+one\n",
                "*** Update File: foo.txt\n@@\n-one\n+two\n",
                "*** Add File: Gone.txt\n+x\n*** Delete File: gone.txt\n",
                "*** Add File: empty/Bar.txt\n+one\n",
                "*** Update File: empty/bar.txt\n@@\n-one\n+two\n*** End Patch\n"
            ))
            .await
            .unwrap();
            assert_eq!(names(), ["Foo.txt", "empty", "readme.md"]);
            assert_eq!(
                std::fs::read_to_string(workspace.join("empty/Bar.txt")).unwrap(),
                "two\n"
            );
            assert_eq!(
                std::fs::read_to_string(workspace.join("Foo.txt")).unwrap(),
                "two\n"
            );
        }
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    /// `HAARNESS_TEST_CASE_SENSITIVE_DIR` names a case-sensitive disk mounted
    /// inside one that ignores case (a case-sensitive APFS image on macOS),
    /// for the checks only such a disk can make.
    #[tokio::test]
    async fn letter_case_is_folded_only_in_folders_that_ignore_it() {
        let folder = std::env::temp_dir().join(format!("haarness-fold-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("Probe.txt"), "").unwrap();
        std::fs::create_dir(folder.join("1")).unwrap();
        let folds = folder.join("PROBE.TXT").exists();
        assert_eq!(folds_case(&folder), folds);
        // An empty folder answers through the folder holding it.
        assert_eq!(folds_case(&folder.join("1")), folds);
        std::fs::remove_dir_all(folder).unwrap();
        let Some(sensitive) = std::env::var_os("HAARNESS_TEST_CASE_SENSITIVE_DIR") else {
            return;
        };
        // The mount itself and a folder without letters below it.
        let sensitive = PathBuf::from(sensitive).canonicalize().unwrap();
        let digits = sensitive.join(std::process::id().to_string());
        std::fs::create_dir(&digits).unwrap();
        assert!(!folds_case(&digits));
        std::fs::write(digits.join("Probe.txt"), "").unwrap();
        assert!(!folds_case(&sensitive));
        assert!(!folds_case(&digits));
        // Two files whose names differ only in case stay two files.
        std::fs::write(digits.join("Foo.txt"), "same\n").unwrap();
        std::fs::write(digits.join("foo.txt"), "same\n").unwrap();
        std::fs::write(digits.join("Gone.txt"), "upper\n").unwrap();
        std::fs::write(digits.join("gone.txt"), "lower\n").unwrap();
        NativeToolRegistry::new(digits.clone())
            .get("apply_patch")
            .unwrap()
            .execute(
                json!({"input": concat!(
                    "*** Begin Patch\n*** Update File: Foo.txt\n@@\n-same\n+upper\n",
                    "*** Update File: foo.txt\n@@\n-same\n+lower\n",
                    "*** Delete File: gone.txt\n*** Update File: Gone.txt\n@@\n-upper\n+UPPER\n",
                    "*** End Patch\n"
                )}),
                tool_context(&digits),
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(digits.join("Foo.txt")).unwrap(),
            "upper\n"
        );
        assert_eq!(
            std::fs::read_to_string(digits.join("foo.txt")).unwrap(),
            "lower\n"
        );
        assert_eq!(
            std::fs::read_to_string(digits.join("Gone.txt")).unwrap(),
            "UPPER\n"
        );
        assert!(!digits.join("gone.txt").exists());
        std::fs::remove_dir_all(digits).unwrap();
    }

    #[tokio::test]
    async fn patches_taking_two_files_in_opposite_orders_never_wait_on_each_other() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-patch-order-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        for name in ["a.txt", "b.txt"] {
            std::fs::write(workspace.join(name), "one\ntwo\n").unwrap();
        }
        let workspace = workspace.canonicalize().unwrap();
        let registry = NativeToolRegistry::new(workspace.clone());
        // Another call holds both files, so both patches queue for them.
        let held = crate::file_lock::lock(
            [workspace.join("a.txt"), workspace.join("b.txt")],
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let run = |input: &'static str| {
            let patch = registry.get("apply_patch").unwrap();
            let context = tool_context(&workspace);
            tokio::spawn(async move { patch.execute(json!({ "input": input }), context).await })
        };
        // As written, one patch names a.txt first and the other b.txt.
        let forward = run(concat!(
            "*** Begin Patch\n*** Update File: a.txt\n@@\n-one\n+ONE\n",
            "*** Update File: b.txt\n@@\n-one\n+ONE\n*** End Patch\n"
        ));
        let backward = run(concat!(
            "*** Begin Patch\n*** Update File: b.txt\n@@\n-two\n+TWO\n",
            "*** Update File: x/../a.txt\n@@\n-two\n+TWO\n*** End Patch\n"
        ));
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(held);
        let (forward, backward) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(forward, backward)
        })
        .await
        .expect("the patches waited on each other");
        forward.unwrap().unwrap();
        backward.unwrap().unwrap();
        for name in ["a.txt", "b.txt"] {
            assert_eq!(
                std::fs::read_to_string(workspace.join(name)).unwrap(),
                "ONE\nTWO\n",
                "{name}"
            );
        }
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    #[tokio::test]
    async fn an_edit_does_not_wait_for_another_edits_diagnostics() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-edit-diagnose-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        std::fs::write(workspace.join("notes.mock"), "alpha\nbeta\n").unwrap();
        // A language server that takes three seconds to start, so the first
        // edit's diagnostics take that long.
        let language = Arc::new(LanguageService::new(
            workspace.clone(),
            haarness_language::LspConfig {
                builtins: false,
                servers: std::collections::BTreeMap::from([(
                    "slow".into(),
                    haarness_language::LspServerConfig {
                        command: vec!["python3".into(), language_fixture("mock_lsp.py")],
                        extensions: vec![".mock".into()],
                        environment: std::collections::BTreeMap::from([(
                            "INIT_DELAY".into(),
                            "3".into(),
                        )]),
                        ..haarness_language::LspServerConfig::default()
                    },
                )]),
                ..haarness_language::LspConfig::default()
            },
            haarness_language::FormatterConfig::default(),
        ));
        let registry = NativeToolRegistry::new(workspace.clone()).with_language(language.clone());
        let edit = |old: &str, new: &str| {
            let edit = registry.get("edit").unwrap();
            let context = tool_context(&workspace);
            let arguments = json!({"path":"notes.mock", "old": old, "new": new});
            tokio::spawn(async move { edit.execute(arguments, context).await })
        };
        let read = || std::fs::read_to_string(workspace.join("notes.mock")).unwrap();
        let first = edit("alpha", "alpha first");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !read().contains("alpha first") {
            assert!(
                std::time::Instant::now() < deadline,
                "the first edit never landed"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // The first edit now waits for the language server; the second edit
        // is written meanwhile.
        let second = edit("beta", "beta second");
        let deadline = std::time::Instant::now() + Duration::from_millis(1_500);
        while !read().contains("beta second") {
            assert!(
                std::time::Instant::now() < deadline,
                "the second edit waited for the first one's diagnostics"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!first.is_finished());
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();
        assert_eq!(read(), "alpha first\nbeta second\n");
        language.shutdown().await;
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn parallel_edits_of_one_file_wait_for_each_others_formatter() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-edit-lock-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        std::fs::write(workspace.join("notes.slow"), "alpha\nbeta\n").unwrap();
        // A formatter that reads the file, takes a second unless the file
        // has the second edit, and writes back what it read.
        let language = Arc::new(LanguageService::new(
            workspace.clone(),
            haarness_language::LspConfig {
                enabled: false,
                ..haarness_language::LspConfig::default()
            },
            haarness_language::FormatterConfig {
                enabled: true,
                builtins: false,
                formatters: std::collections::BTreeMap::from([(
                    "slow".into(),
                    haarness_language::FormatterDefinition {
                        command: vec![
                            "sh".into(),
                            "-c".into(),
                            r#"text=$(cat "$1"); case "$text" in *second*) ;; *) sleep 1 ;; esac; printf '%s\n' "$text" > "$1""#.into(),
                            "sh".into(),
                            "$FILE".into(),
                        ],
                        extensions: vec![".slow".into()],
                        ..haarness_language::FormatterDefinition::default()
                    },
                )]),
            },
        ));
        let registry = NativeToolRegistry::new(workspace.clone()).with_language(language);
        let edit = registry.get("edit").unwrap();
        let patch = registry.get("apply_patch").unwrap();
        let first = edit.execute(
            json!({"path":"notes.slow", "old":"alpha", "new":"alpha first"}),
            tool_context(&workspace),
        );
        let second = async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            patch
                .execute(
                    json!({"input":"*** Begin Patch\n*** Update File: notes.slow\n@@\n-beta\n+beta second\n*** End Patch\n"}),
                    tool_context(&workspace),
                )
                .await
        };
        let (first, second) = tokio::join!(first, second);
        first.unwrap();
        second.unwrap();
        assert_eq!(
            std::fs::read_to_string(workspace.join("notes.slow")).unwrap(),
            "alpha first\nbeta second\n"
        );

        // A call waiting for a file another call holds ends on Stop.
        let held = crate::file_lock::lock(
            [workspace.join("notes.slow").canonicalize().unwrap()],
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let context = tool_context(&workspace);
        let stop = context.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            stop.cancel();
        });
        let stopped = edit
            .execute(
                json!({"path":"notes.slow", "old":"alpha", "new":"lost"}),
                context,
            )
            .await;
        assert!(matches!(stopped, Err(CoreError::Cancelled)), "{stopped:?}");
        drop(held);
        assert_eq!(
            std::fs::read_to_string(workspace.join("notes.slow")).unwrap(),
            "alpha first\nbeta second\n"
        );
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    #[tokio::test]
    async fn filesystem_tools_round_trip_and_reject_workspace_escape() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-tools-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        let registry = NativeToolRegistry::new(workspace.clone());
        let context = ToolContext {
            execution: haarness_core::ExecutionSettings::default(),
            thread_id: ThreadId::new(),
            turn_id: TurnId::new(),
            workspace: workspace.clone(),
            authorized_external_paths: Vec::new(),
            cancel: CancellationToken::new(),
            invoker: None,
        };

        let write = registry
            .get("write")
            .unwrap()
            .execute(
                json!({"path":"notes/test.txt","content":"alpha\nbeta\n"}),
                context.clone(),
            )
            .await
            .unwrap();
        let diffs: Vec<FileDiff> = serde_json::from_value(write.metadata["diffs"].clone()).unwrap();
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].status, FileDiffStatus::Added);
        assert_eq!(diffs[0].before, None);
        assert_eq!(diffs[0].after.as_deref(), Some("alpha\nbeta\n"));

        let overwrite = registry
            .get("write")
            .unwrap()
            .execute(
                json!({"path":"notes/test.txt","content":"alpha\nbeta\nomega\n"}),
                context.clone(),
            )
            .await
            .unwrap();
        let diffs: Vec<FileDiff> =
            serde_json::from_value(overwrite.metadata["diffs"].clone()).unwrap();
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].status, FileDiffStatus::Modified);
        assert_eq!(diffs[0].before.as_deref(), Some("alpha\nbeta\n"));
        assert_eq!(diffs[0].after.as_deref(), Some("alpha\nbeta\nomega\n"));
        let read = registry
            .get("read")
            .unwrap()
            .execute(json!({"path":"notes/test.txt"}), context.clone())
            .await
            .unwrap();
        assert_eq!(read.text, "alpha\nbeta\nomega");

        let edit = registry
            .get("edit")
            .unwrap()
            .execute(
                json!({"path":"notes/test.txt","old":"beta","new":"gamma"}),
                context.clone(),
            )
            .await
            .unwrap();
        let diffs: Vec<FileDiff> = serde_json::from_value(edit.metadata["diffs"].clone()).unwrap();
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].status, FileDiffStatus::Modified);
        assert_eq!(diffs[0].before.as_deref(), Some("alpha\nbeta\nomega\n"));
        assert_eq!(diffs[0].after.as_deref(), Some("alpha\ngamma\nomega\n"));
        assert_eq!(
            tokio::fs::read_to_string(workspace.join("notes/test.txt"))
                .await
                .unwrap(),
            "alpha\ngamma\nomega\n"
        );

        let escaped = registry
            .get("read")
            .unwrap()
            .execute(json!({"path":"../"}), context)
            .await;
        assert!(matches!(escaped, Err(CoreError::PermissionDenied(_))));

        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    #[test]
    fn mutation_diff_counts_changed_lines_not_the_edited_region() {
        // One edit adds an import, changes a signature, inserts a limiter
        // block and replaces a route; the lines between them stay as they
        // were. `git diff --numstat` says 10 added, 2 removed.
        let before = "const express = require(\"express\");\n\
            const { listUsers } = require(\"./users\");\n\
            \n\
            const app = express();\n\
            \n\
            function createServer(port) {\n\
            \x20 app.use(express.json());\n\
            \x20 app.get(\"/health\", (req, res) => res.send(\"ok\"));\n\
            \x20 app.get(\"/users\", (req, res) => res.json(listUsers()));\n\
            \x20 return app.listen(port);\n\
            }\n\
            \n\
            module.exports = { createServer };\n";
        let after = "const express = require(\"express\");\n\
            const rateLimit = require(\"express-rate-limit\");\n\
            const { listUsers } = require(\"./users\");\n\
            \n\
            const app = express();\n\
            \n\
            function createServer(port, { limit = 100 } = {}) {\n\
            \x20 app.use(express.json());\n\
            \x20 const limiter = rateLimit({\n\
            \x20   windowMs: 60 * 1000,\n\
            \x20   max: limit,\n\
            \x20   standardHeaders: true,\n\
            \x20   legacyHeaders: false,\n\
            \x20 });\n\
            \x20 app.use(\"/users\", limiter);\n\
            \x20 app.get(\"/health\", (req, res) => res.send(\"ok\"));\n\
            \x20 app.get(\"/users\", limiter, (req, res) => res.json(listUsers(req.query)));\n\
            \x20 return app.listen(port);\n\
            }\n\
            \n\
            module.exports = { createServer };\n";
        let diff = text_file_diff(
            "src/app.js",
            FileDiffStatus::Modified,
            Some(before),
            Some(after),
        );
        assert_eq!((diff.additions, diff.deletions), (10, 2));

        // Only the final newline changed: still a one-line mutation.
        let diff = text_file_diff(
            "a.txt",
            FileDiffStatus::Modified,
            Some("a\nb"),
            Some("a\nb\n"),
        );
        assert_eq!((diff.additions, diff.deletions), (1, 1));
        let diff = text_file_diff("a.txt", FileDiffStatus::Modified, Some("a\n"), Some("a\n"));
        assert_eq!((diff.additions, diff.deletions), (0, 0));
    }

    #[test]
    fn mutation_diff_metadata_is_bounded_and_snapshot_scoped() {
        let oversized = "é".repeat(TOOL_DIFF_BODY_MAX_BYTES);
        let diff = text_file_diff(
            "large.txt",
            FileDiffStatus::Modified,
            Some(&oversized),
            Some(&oversized.replace('é', "x")),
        );
        assert!(diff.truncated);
        assert!(diff.before.as_ref().unwrap().len() <= TOOL_DIFF_BODY_MAX_BYTES);
        assert!(diff.after.as_ref().unwrap().len() <= TOOL_DIFF_BODY_MAX_BYTES);
        assert!(
            diff.before
                .as_ref()
                .unwrap()
                .is_char_boundary(diff.before.as_ref().unwrap().len())
        );

        let workspace = PathBuf::from("/tmp/haarness-side-effect-classification");
        let registry = NativeToolRegistry::new(workspace);
        for name in ["write", "edit", "apply_patch", "bash"] {
            assert!(registry.get(name).unwrap().side_effectful(), "{name}");
        }
        let bash = registry.get("bash").unwrap();
        assert!(!bash.side_effectful_for(&json!({"command":"true"})));
        assert!(!bash.side_effectful_for(&json!({"command":"sleep 1"})));
        assert!(bash.side_effectful_for(&json!({"command":"sleep 1; touch changed"})));
        assert!(!bash.side_effectful_for(&json!({"command":"git status --short"})));
        assert!(!bash.side_effectful_for(&json!({"command":"find crates -type f | sort"})));
        assert!(bash.side_effectful_for(&json!({"command":"find crates -delete"})));
        assert!(bash.side_effectful_for(&json!({"command":"cargo test"})));
        assert!(
            !registry.get("task").unwrap().side_effectful(),
            "child turns own their workspace mutation snapshots"
        );
    }

    #[tokio::test]
    async fn filesystem_tools_accept_only_the_external_path_granted_for_the_call() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-tools-workspace-{}", uuid::Uuid::now_v7()));
        let external =
            std::env::temp_dir().join(format!("haarness-tools-external-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::create_dir_all(&external).await.unwrap();
        tokio::fs::write(external.join("visible.txt"), "approved\n")
            .await
            .unwrap();
        let registry = NativeToolRegistry::new(workspace.clone());
        let mut context = ToolContext {
            execution: haarness_core::ExecutionSettings::default(),
            thread_id: ThreadId::new(),
            turn_id: TurnId::new(),
            workspace: workspace.clone(),
            authorized_external_paths: Vec::new(),
            cancel: CancellationToken::new(),
            invoker: None,
        };

        let denied = registry
            .get("list")
            .unwrap()
            .execute(json!({"path": external}), context.clone())
            .await;
        assert!(matches!(denied, Err(CoreError::PermissionDenied(_))));

        context.authorized_external_paths.push(
            external
                .canonicalize()
                .expect("external test directory canonicalizes"),
        );
        let allowed = registry
            .get("list")
            .unwrap()
            .execute(json!({"path": external}), context)
            .await
            .unwrap();
        assert_eq!(allowed.text, "visible.txt");

        let target = external.join("created.txt");
        let write_context = ToolContext {
            execution: haarness_core::ExecutionSettings::default(),
            thread_id: ThreadId::new(),
            turn_id: TurnId::new(),
            workspace: workspace.clone(),
            authorized_external_paths: vec![haarness_core::resolve_permission_path(
                &workspace,
                &target.to_string_lossy(),
            )],
            cancel: CancellationToken::new(),
            invoker: None,
        };
        registry
            .get("write")
            .unwrap()
            .execute(
                json!({"path": target, "content": "allowed once\n"}),
                write_context,
            )
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read_to_string(external.join("created.txt"))
                .await
                .unwrap(),
            "allowed once\n"
        );

        tokio::fs::remove_dir_all(external).await.unwrap();
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    #[tokio::test]
    async fn grep_respects_gitignore_and_hidden_files() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-grep-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(workspace.join(".git"))
            .await
            .unwrap();
        tokio::fs::write(workspace.join(".gitignore"), "ignored.txt\n")
            .await
            .unwrap();
        tokio::fs::write(workspace.join("visible.txt"), "needle visible\n")
            .await
            .unwrap();
        tokio::fs::write(workspace.join("ignored.txt"), "needle ignored\n")
            .await
            .unwrap();
        tokio::fs::write(workspace.join(".hidden.txt"), "needle hidden\n")
            .await
            .unwrap();
        let registry = NativeToolRegistry::new(workspace.clone());
        let context = ToolContext {
            execution: haarness_core::ExecutionSettings::default(),
            thread_id: ThreadId::new(),
            turn_id: TurnId::new(),
            workspace: workspace.clone(),
            authorized_external_paths: Vec::new(),
            cancel: CancellationToken::new(),
            invoker: None,
        };

        let result = registry
            .get("grep")
            .unwrap()
            .execute(json!({"pattern":"needle","path":"."}), context)
            .await
            .unwrap();
        assert!(result.text.contains("visible.txt"), "{}", result.text);
        assert!(!result.text.contains("ignored.txt"), "{}", result.text);
        assert!(!result.text.contains("hidden.txt"), "{}", result.text);
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn search_tools_skip_secret_named_files() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-secrets-{}", uuid::Uuid::now_v7()));
        for directory in [
            ".git",
            "node_modules/pkg",
            "node_modules/@s",
            "packages/pkg",
        ] {
            std::fs::create_dir_all(workspace.join(directory)).unwrap();
        }
        std::fs::create_dir_all(workspace.join("conf.env.d")).unwrap();
        // Results name paths relative to the workspace only under its real path.
        let workspace = workspace.canonicalize().unwrap();
        // Every file holds the needle and its own name.
        for file in [
            ".env",
            "prod.env",
            "app.env.local",
            ".env.example",
            "c.env.example",
            "d.txt",
            "node_modules/pkg/e.env",
            "conf.env.d/notes.txt",
            "packages/pkg/prod.env",
            "packages/pkg/index.ts",
        ] {
            std::fs::write(workspace.join(file), format!("needle {file}\n")).unwrap();
        }
        std::os::unix::fs::symlink(".env", workspace.join("notes.txt")).unwrap();
        // A workspace package linked into node_modules keeps its own secrets.
        std::os::unix::fs::symlink("../../packages/pkg", workspace.join("node_modules/@s/pkg"))
            .unwrap();
        let registry = NativeToolRegistry::new(workspace.clone());
        let run = |tool: &'static str, args: Value| {
            let registry = &registry;
            let context = ToolContext {
                execution: haarness_core::ExecutionSettings::default(),
                thread_id: ThreadId::new(),
                turn_id: TurnId::new(),
                workspace: workspace.clone(),
                authorized_external_paths: Vec::new(),
                cancel: CancellationToken::new(),
                invoker: None,
            };
            async move {
                registry
                    .get(tool)
                    .unwrap()
                    .execute(args, context)
                    .await
                    .unwrap()
            }
        };
        let none = |count: usize| {
            format!(
                "No files found\n\n(skipped {count} file{} with a secret's name)",
                if count == 1 { "" } else { "s" }
            )
        };

        // A tree walk leaves secret-named files out and counts them; hidden
        // files and links are not walked at all.
        let grep = run("grep", json!({"pattern":"needle"})).await;
        for kept in [
            "d.txt:1:needle d.txt",
            "c.env.example:1:",
            "node_modules/pkg/e.env:1:",
            "packages/pkg/index.ts:1:",
        ] {
            assert!(grep.text.contains(kept), "{kept}: {}", grep.text);
        }
        for secret in [
            "needle prod.env",
            "needle app.env.local",
            "needle .env\n",
            "needle conf.env.d",
            "needle packages/pkg/prod.env",
        ] {
            assert!(!grep.text.contains(secret), "{secret}: {}", grep.text);
        }
        assert!(
            grep.text
                .ends_with("\n\n(skipped 4 files with a secret's name)"),
            "{}",
            grep.text
        );
        assert_eq!(grep.metadata["skipped"], 4);
        // A secret-named file named directly, or a link to one, is never read.
        for path in [".env", "prod.env", "notes.txt", "conf.env.d"] {
            let result = run("grep", json!({"pattern":"needle","path":path})).await;
            assert_eq!(result.text, none(1), "{path}");
            assert_eq!(result.metadata["skipped"], 1, "{path}");
        }
        for path in [".env.example", "node_modules/pkg/e.env"] {
            let result = run("grep", json!({"pattern":"needle","path":path})).await;
            assert!(result.text.contains(&format!("needle {path}")), "{path}");
            assert!(result.metadata.is_null(), "{path}");
        }
        // A linked package is judged by its real folder too.
        let linked = run(
            "grep",
            json!({"pattern":"needle","path":"node_modules/@s/pkg"}),
        )
        .await;
        assert!(linked.text.contains("needle packages/pkg/index.ts"));
        assert!(!linked.text.contains("needle packages/pkg/prod.env"));
        assert!(
            linked
                .text
                .ends_with("(skipped 1 file with a secret's name)")
        );
        // No match: still says what it skipped.
        let empty = run("grep", json!({"pattern":"absent"})).await;
        assert_eq!(empty.text, none(4));

        // glob matches dotfiles, so the project's .env is counted here.
        let glob = run("glob", json!({"pattern":"*"})).await;
        let names = glob.text.lines().collect::<Vec<_>>();
        for kept in [
            ".env.example",
            "c.env.example",
            "d.txt",
            "conf.env.d",
            "node_modules",
        ] {
            assert!(names.contains(&kept), "{kept}: {}", glob.text);
        }
        for secret in [".env", "prod.env", "app.env.local", "notes.txt"] {
            assert!(!names.contains(&secret), "{secret}: {}", glob.text);
        }
        assert!(
            glob.text
                .ends_with("\n\n(skipped 4 files with a secret's name)")
        );
        assert_eq!(glob.metadata["skipped"], 4);
        assert_eq!(run("glob", json!({"pattern":"*.env"})).await.text, none(2));
        let nested = run("glob", json!({"pattern":"node_modules/**/*.env"})).await;
        assert_eq!(
            nested.text,
            "node_modules/pkg/e.env\n\n(skipped 1 file with a secret's name)"
        );

        let list = run("list", json!({"path":"."})).await;
        let names = list.text.lines().collect::<Vec<_>>();
        for kept in [
            ".env.example",
            "c.env.example",
            "d.txt",
            "conf.env.d",
            "packages",
        ] {
            assert!(names.contains(&kept), "{kept}: {}", list.text);
        }
        for secret in [".env", "prod.env", "app.env.local", "notes.txt"] {
            assert!(!names.contains(&secret), "{secret}: {}", list.text);
        }
        assert!(
            list.text
                .ends_with("\n\n(skipped 4 files with a secret's name)")
        );
        assert_eq!(list.metadata["skipped"], 4);
        let package = run("list", json!({"path":"node_modules/pkg"})).await;
        assert_eq!(package.text, "e.env");
        let linked = run("list", json!({"path":"node_modules/@s/pkg"})).await;
        assert_eq!(
            linked.text,
            "index.ts\n\n(skipped 1 file with a secret's name)"
        );
        assert_eq!(
            run("list", json!({"path":"conf.env.d"})).await.text,
            none(1)
        );
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    /// An agent turn whose rules deny reading `private/*`.
    struct DenyPrivate(PathBuf);

    #[async_trait]
    impl haarness_core::ToolInvoker for DenyPrivate {
        async fn invoke(&self, _name: &str, _arguments: Value) -> CoreResult<ToolOutput> {
            unreachable!("search tools never re-enter")
        }
        fn side_effectful_for(&self, _name: &str, _arguments: &Value) -> CoreResult<bool> {
            Ok(false)
        }
        fn read_rules(&self) -> Option<haarness_core::PermissionEngine> {
            let mut rules = haarness_core::PermissionEngine::build_defaults();
            rules.add_rule(haarness_core::PermissionRule {
                pattern: "read:private/*".into(),
                decision: haarness_core::PermissionDecision::Deny,
            });
            rules.set_workspace(&self.0);
            Some(rules)
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn searches_leave_out_files_a_read_rule_denies_and_say_how_many() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-search-denied-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(workspace.join("private")).unwrap();
        std::fs::create_dir_all(workspace.join("public")).unwrap();
        // Search output names files relative to the workspace as configured.
        let workspace = workspace.canonicalize().unwrap();
        for file in [
            "private/key.txt",
            "private/notes.md",
            "public/readme.md",
            "public/.env",
        ] {
            std::fs::write(workspace.join(file), format!("needle {file}\n")).unwrap();
        }
        std::os::unix::fs::symlink("../private/key.txt", workspace.join("public/link.txt"))
            .unwrap();
        let registry = NativeToolRegistry::new(workspace.clone());
        let run = |tool: &'static str, args: Value, rules: bool| {
            let registry = &registry;
            let mut context = tool_context(&workspace);
            if rules {
                context.invoker = Some(Arc::new(DenyPrivate(workspace.clone())));
            }
            async move {
                registry
                    .get(tool)
                    .unwrap()
                    .execute(args, context)
                    .await
                    .unwrap()
            }
        };
        let grep = run("grep", json!({"pattern":"needle","path":"."}), true).await;
        assert!(
            grep.text.contains("public/readme.md:1:needle"),
            "{}",
            grep.text
        );
        assert!(!grep.text.contains("needle private/"), "{}", grep.text);
        assert!(
            grep.text.ends_with("(skipped 2 files a read rule denies)"),
            "{}",
            grep.text
        );
        assert_eq!(grep.metadata["denied"], 2);
        assert!(grep.metadata.get("skipped").is_none());
        // Named directly, the file is never read.
        let direct = run(
            "grep",
            json!({"pattern":"needle","path":"private/key.txt"}),
            true,
        )
        .await;
        assert_eq!(
            direct.text,
            "No files found\n\n(skipped 1 file a read rule denies)"
        );
        // glob matches dotfiles and links: a secret and a link to a denied
        // file are counted under their own reasons.
        let glob = run("glob", json!({"pattern":"**/*"}), true).await;
        let names = glob.text.lines().collect::<Vec<_>>();
        assert!(names.contains(&"public/readme.md"), "{}", glob.text);
        for hidden in [
            "private/key.txt",
            "private/notes.md",
            "public/link.txt",
            "public/.env",
        ] {
            assert!(!names.contains(&hidden), "{hidden}: {}", glob.text);
        }
        assert!(
            glob.text
                .ends_with("(skipped 1 file with a secret's name and 3 files a read rule denies)"),
            "{}",
            glob.text
        );
        assert_eq!(glob.metadata["skipped"], 1);
        assert_eq!(glob.metadata["denied"], 3);
        // Without an agent's rules (a direct call), only secret names are left out.
        let plain = run("grep", json!({"pattern":"needle","path":"."}), false).await;
        assert!(
            plain.text.contains("needle private/key.txt"),
            "{}",
            plain.text
        );
        std::fs::remove_dir_all(workspace).unwrap();
    }

    #[tokio::test]
    async fn searches_and_listings_leave_out_where_sign_ins_are_kept() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-search-sign-ins-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(workspace.join("data")).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        for file in [
            "data/auth.json",
            "data/auth.json.lock",
            "data/notes.md",
            "auth.json",
        ] {
            std::fs::write(workspace.join(file), format!("needle {file}\n")).unwrap();
        }
        haarness_core::record_credential_store(workspace.join("data/auth.json"), false);
        let registry = NativeToolRegistry::new(workspace.clone());
        let run = |tool: &'static str, args: Value| {
            let registry = &registry;
            let context = tool_context(&workspace);
            async move {
                registry
                    .get(tool)
                    .unwrap()
                    .execute(args, context)
                    .await
                    .unwrap()
            }
        };
        let grep = run("grep", json!({"pattern":"needle","path":"."})).await;
        assert!(
            grep.text.contains("data/notes.md:1:needle"),
            "{}",
            grep.text
        );
        // The project's own `auth.json` is an ordinary file.
        assert!(
            grep.text.contains("auth.json:1:needle auth.json"),
            "{}",
            grep.text
        );
        assert!(
            !grep.text.contains("needle data/auth.json"),
            "{}",
            grep.text
        );
        assert!(
            grep.text
                .ends_with("(skipped 2 files where Haarness keeps sign-ins)"),
            "{}",
            grep.text
        );
        assert_eq!(grep.metadata["signIns"], 2);
        let direct = run("grep", json!({"pattern":"needle","path":"data/auth.json"})).await;
        assert_eq!(
            direct.text,
            "No files found\n\n(skipped 1 file where Haarness keeps sign-ins)"
        );
        let list = run("list", json!({"path":"data"})).await;
        assert_eq!(
            list.text,
            "notes.md\n\n(skipped 2 files where Haarness keeps sign-ins)"
        );
        let glob = run("glob", json!({"pattern":"data/*"})).await;
        assert!(!glob.text.contains("auth.json"), "{}", glob.text);
        std::fs::remove_dir_all(workspace).unwrap();
    }

    #[tokio::test]
    async fn a_glob_stopped_at_its_limit_says_its_skip_count_is_a_lower_bound() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-glob-cap-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&workspace).unwrap();
        // glob lists names in order, so a.env is met before the limit.
        std::fs::write(workspace.join("a.env"), "").unwrap();
        for index in 0..10_001 {
            std::fs::write(workspace.join(format!("f{index:05}.txt")), "").unwrap();
        }
        let result = NativeToolRegistry::new(workspace.clone())
            .get("glob")
            .unwrap()
            .execute(
                json!({"pattern":"*"}),
                ToolContext {
                    execution: haarness_core::ExecutionSettings::default(),
                    thread_id: ThreadId::new(),
                    turn_id: TurnId::new(),
                    workspace: workspace.clone(),
                    authorized_external_paths: Vec::new(),
                    cancel: CancellationToken::new(),
                    invoker: None,
                },
            )
            .await
            .unwrap();
        assert!(
            result
                .text
                .ends_with("\n\n(skipped at least 1 file with a secret's name)"),
            "{}",
            &result.text[result.text.len().saturating_sub(300)..]
        );
        assert_eq!(result.metadata["skipped"], 1);
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_glob_stays_in_the_folder_it_searches() {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("haarness-glob-scope-{}", uuid::Uuid::now_v7()));
        let workspace = root.join("workspace");
        std::fs::create_dir_all(workspace.join("src/inner")).unwrap();
        std::fs::create_dir_all(workspace.join("docs")).unwrap();
        std::fs::create_dir_all(root.join("outside")).unwrap();
        std::fs::write(root.join("outside/elsewhere.txt"), "").unwrap();
        std::fs::write(workspace.join("src/inner/main.rs"), "").unwrap();
        std::fs::write(workspace.join("docs/guide.md"), "").unwrap();
        std::os::unix::fs::symlink("../outside", workspace.join("escape")).unwrap();
        std::os::unix::fs::symlink("../docs", workspace.join("src/docs")).unwrap();
        std::os::unix::fs::symlink(
            root.join("outside/elsewhere.txt"),
            workspace.join("note.txt"),
        )
        .unwrap();
        let registry = NativeToolRegistry::new(workspace.clone());
        let glob = |arguments: Value| {
            let registry = &registry;
            let context = tool_context(&workspace);
            async move {
                registry
                    .get("glob")
                    .unwrap()
                    .execute(arguments, context)
                    .await
            }
        };
        let error = glob(json!({"pattern":"../*"})).await.unwrap_err();
        assert!(error.to_string().contains("cannot contain `..`"), "{error}");
        let error = glob(json!({"pattern":"src/../../outside/*"}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cannot contain `..`"), "{error}");
        // `.*` also matches `..`, and `**` follows linked folders.
        let dotted = glob(json!({"pattern":".*/*"})).await.unwrap();
        assert!(!dotted.text.contains("outside"), "{}", dotted.text);
        let dotted = glob(json!({"pattern":".*/*","path":"src"})).await.unwrap();
        assert!(!dotted.text.contains(".."), "{}", dotted.text);
        let all = glob(json!({"pattern":"**/*"})).await.unwrap();
        assert!(!all.text.contains("elsewhere.txt"), "{}", all.text);
        // The model is told they were left out, not that they are absent.
        assert!(
            all.text
                .ends_with("\n\n(skipped 1 match inside links that lead outside the project)"),
            "{}",
            all.text
        );
        assert_eq!(all.metadata["outside"], 1);
        let none = glob(json!({"pattern":"escape/*"})).await.unwrap();
        assert_eq!(
            none.text,
            "No files found\n\n(skipped 1 match inside links that lead outside the project)"
        );
        // A link in the project is listed, and a link to another project
        // folder is followed.
        assert!(
            all.text.lines().any(|line| line == "note.txt"),
            "{}",
            all.text
        );
        assert!(
            all.text.lines().any(|line| line == "escape"),
            "{}",
            all.text
        );
        assert!(
            all.text.lines().any(|line| line == "src/docs/guide.md"),
            "{}",
            all.text
        );
        let nested = glob(json!({"pattern":"**/*.md","path":"src"}))
            .await
            .unwrap();
        assert_eq!(nested.text, "src/docs/guide.md");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_glob_through_a_link_out_of_the_project_stops_at_its_budget_and_on_stop() {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("haarness-glob-budget-{}", uuid::Uuid::now_v7()));
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(root.join("outside")).unwrap();
        for index in 0..30 {
            std::fs::write(root.join(format!("outside/{index}.txt")), "").unwrap();
        }
        std::fs::write(workspace.join("inside.txt"), "").unwrap();
        std::os::unix::fs::symlink("../outside", workspace.join("escape")).unwrap();
        let scope = GlobScope {
            root: workspace.clone(),
            workspace: workspace.clone(),
            named: workspace.clone(),
        };
        let secrets = SecretFilter::new(&workspace, ".", &workspace, None);
        let pattern = workspace.join("**/*").to_string_lossy().into_owned();
        let cancel = CancellationToken::new();
        // Matches left out count against the budget too.
        let found = glob_walk(&pattern, &scope, &secrets, &cancel, 10).unwrap();
        assert!(found.stopped && found.partial);
        assert!(found.skipped.outside >= 7, "{}", found.skipped.outside);
        assert!(found.values.len() + found.skipped.outside <= 10);
        let everything = glob_walk(&pattern, &scope, &secrets, &cancel, 1_000).unwrap();
        assert!(!everything.stopped && !everything.partial);
        assert_eq!(everything.skipped.outside, 30);
        cancel.cancel();
        assert!(matches!(
            glob_walk(&pattern, &scope, &secrets, &cancel, 1_000),
            Err(CoreError::Cancelled)
        ));
        let stopped = with_stop(output(String::new()), 10);
        assert_eq!(
            stopped.text,
            "(stopped after looking at 10 matches; narrow the pattern or the path)"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn large_tool_output_keeps_a_preview_and_saves_the_full_body() {
        let directory =
            std::env::temp_dir().join(format!("haarness-output-{}", uuid::Uuid::now_v7()));
        let body = (0..3_000)
            .map(|line| format!("line {line}: {}", "x".repeat(40)))
            .collect::<Vec<_>>()
            .join("\n");
        let result = truncate_output_at(body.clone(), &directory).await.unwrap();
        assert_eq!(result.metadata["truncated"], true);
        let path = PathBuf::from(result.metadata["outputPath"].as_str().unwrap());
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), body);
        assert!(result.text.contains("Full output saved to:"));
        assert!(result.text.contains("line 0:"), "{}", result.text);
        assert!(
            !result.text.contains("line 2999:"),
            "head preview kept the tail"
        );
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[test]
    fn a_workflow_path_from_the_home_folder_is_read_there() {
        let home = haarness_protocol::home_dir().unwrap();
        let workspace = Path::new("/work/project");
        assert_eq!(
            workflow_path(workspace, "~/flows/check.ts"),
            home.join("flows/check.ts").display().to_string()
        );
        assert_eq!(
            workflow_path(workspace, "flows/check.ts"),
            "/work/project/flows/check.ts"
        );
        assert_eq!(
            workflow_path(workspace, "./~/check.ts"),
            "/work/project/./~/check.ts"
        );
    }

    #[tokio::test]
    async fn a_preview_shows_the_head_of_a_first_line_too_long_for_it() {
        let directory =
            std::env::temp_dir().join(format!("haarness-output-{}", uuid::Uuid::now_v7()));
        let body = format!("{}\nsecond", "y".repeat(200_000));
        let result = truncate_output_at(body, &directory).await.unwrap();
        assert!(
            result
                .text
                .starts_with(&format!("{}{}", "y".repeat(2_000), text::LINE_CUT)),
            "{}",
            &result.text[..80]
        );
        assert!(result.text.contains("Full output saved to:"));
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    fn read_fixture(name: &str, body: &[u8]) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("haarness-read-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join(name), body).unwrap();
        (root.clone(), root.join(name))
    }

    async fn read_text_of(root: &Path, args: Value) -> CoreResult<ToolOutput> {
        let registry = NativeToolRegistry::new(root.to_path_buf());
        registry
            .get("read")
            .unwrap()
            .execute(args, tool_context(root))
            .await
    }

    #[tokio::test]
    async fn read_shows_the_head_of_a_line_that_never_ends() {
        let root = std::env::temp_dir().join(format!("haarness-read-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        // 40 MiB on one line, as a minified bundle or a JSON dump.
        {
            use std::io::Write as _;
            let mut file = std::fs::File::create(root.join("bundle.min.js")).unwrap();
            let chunk = "x".repeat(1024 * 1024);
            for _ in 0..40 {
                file.write_all(chunk.as_bytes()).unwrap();
            }
            file.write_all(b"\nlast line\n").unwrap();
        }
        let result = read_text_of(&root, json!({"path":"bundle.min.js"}))
            .await
            .unwrap();
        assert_eq!(
            result.text,
            format!("{}{}\nlast line", "x".repeat(2_000), text::LINE_CUT)
        );
        // Shown inline: nothing is copied to the tool output folder.
        assert!(result.metadata.get("outputPath").is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn read_stops_at_fifty_kilobytes_and_says_where_to_go_on() {
        let body = (0..3_000)
            .map(|line| format!("line {line:04} {}", "z".repeat(90)))
            .collect::<Vec<_>>()
            .join("\n");
        let (root, _) = read_fixture("long.txt", body.as_bytes());
        let first = read_text_of(&root, json!({"path":"long.txt","limit":3000}))
            .await
            .unwrap();
        let (shown, note) = first.text.rsplit_once("\n\n").unwrap();
        assert!(shown.len() <= 50 * 1024, "{}", shown.len());
        let count = shown.lines().count();
        assert!(count > 400 && count < 3_000, "{count}");
        assert_eq!(
            note,
            format!("(stopped at 50 KB; read on with offset={count})")
        );
        assert!(first.metadata.get("outputPath").is_none());
        let next = read_text_of(&root, json!({"path":"long.txt","offset":count,"limit":1}))
            .await
            .unwrap();
        assert_eq!(
            next.text,
            format!(
                "{}\n\n(more lines follow; read on with offset={})",
                body.lines().nth(count).unwrap(),
                count + 1
            )
        );
        // A limit, the default one or one the call gives, ending before the
        // file does says where to go on.
        let short = (0..2_500)
            .map(|line| format!("{line}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(root.join("short.txt"), &short).unwrap();
        let default = read_text_of(&root, json!({"path":"short.txt"}))
            .await
            .unwrap();
        assert!(
            default
                .text
                .ends_with("1999\n\n(more lines follow; read on with offset=2000)"),
            "{}",
            &default.text[default.text.len() - 80..]
        );
        let given = read_text_of(&root, json!({"path":"short.txt","limit":500}))
            .await
            .unwrap();
        assert!(
            given
                .text
                .ends_with("499\n\n(more lines follow; read on with offset=500)"),
            "{}",
            &given.text[given.text.len() - 60..]
        );
        let rest = read_text_of(&root, json!({"path":"short.txt","offset":2000}))
            .await
            .unwrap();
        assert!(rest.text.ends_with("\n2499") && !rest.text.contains("more lines"));
        // A limit inside the budget reads the lines it always did.
        let small = read_text_of(&root, json!({"path":"long.txt","offset":10,"limit":5}))
            .await
            .unwrap();
        assert_eq!(
            small.text,
            format!(
                "{}\n\n(more lines follow; read on with offset=15)",
                body.lines().skip(10).take(5).collect::<Vec<_>>().join("\n")
            )
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn read_splits_lines_as_it_always_did() {
        for body in [
            "one\ntwo\nthree\n",
            "one\r\ntwo\r\nthree",
            "\n\nthird\n\n",
            "",
            "lone\rreturn\n",
        ] {
            let (root, _) = read_fixture("f.txt", body.as_bytes());
            for (offset, limit) in [(0, 2000), (1, 1), (2, 5), (9, 3)] {
                let result =
                    read_text_of(&root, json!({"path":"f.txt","offset":offset,"limit":limit}))
                        .await
                        .unwrap();
                let shown = body.lines().skip(offset).take(limit).count();
                let mut expected = body
                    .lines()
                    .skip(offset)
                    .take(limit)
                    .collect::<Vec<_>>()
                    .join("\n");
                if body.lines().count() > offset + limit {
                    expected = format!(
                        "{expected}\n\n(more lines follow; read on with offset={})",
                        offset + shown
                    );
                }
                assert_eq!(result.text, expected, "{body:?} {offset} {limit}");
            }
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test]
    async fn a_long_cjk_or_accented_line_reads_as_utf8() {
        let body = format!("{}\na{}\nend\n", "日".repeat(3_000), "é".repeat(5_000));
        let (root, _) = read_fixture("wide.txt", body.as_bytes());
        let result = read_text_of(&root, json!({"path":"wide.txt"}))
            .await
            .unwrap();
        assert_eq!(
            result.text,
            format!(
                "{}{cut}\na{}{cut}\nend",
                "日".repeat(2_000),
                "é".repeat(1_999),
                cut = text::LINE_CUT
            )
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn read_shows_text_that_is_not_utf8_and_refuses_binary_data() {
        let (root, _) = read_fixture("latin1.txt", b"caf\xe9\nna\xefve\n");
        let result = read_text_of(&root, json!({"path":"latin1.txt"}))
            .await
            .unwrap();
        assert_eq!(
            result.text,
            "caf\u{fffd}\nna\u{fffd}ve\n\n(this file is not UTF-8: bytes that do not decode show as \u{fffd}; edit cannot change it, and write would replace those bytes)"
        );
        let refused = NativeToolRegistry::new(root.clone())
            .get("edit")
            .unwrap()
            .execute(
                json!({"path":"latin1.txt","old":"na","new":"no"}),
                tool_context(&root),
            )
            .await
            .unwrap_err();
        assert!(
            refused
                .to_string()
                .contains("latin1.txt is not UTF-8 text, so edit cannot change it"),
            "{refused}"
        );
        // A NUL near the start marks binary data: only UTF-8 shows, as before.
        std::fs::write(root.join("blob.bin"), b"\x00\x01\xff\xfe").unwrap();
        let refused = read_text_of(&root, json!({"path":"blob.bin"}))
            .await
            .unwrap_err();
        assert!(
            refused.to_string().contains("holds binary data"),
            "{refused}"
        );
        std::fs::write(root.join("nul.txt"), b"a\x00b\n").unwrap();
        let shown = read_text_of(&root, json!({"path":"nul.txt"}))
            .await
            .unwrap();
        assert_eq!(shown.text, "a\u{0}b");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn read_refuses_a_pipe_rather_than_waiting_on_it() {
        let root = std::env::temp_dir().join(format!("haarness-read-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(root.join("pipe"))
                .status()
                .unwrap()
                .success()
        );
        let refused = read_text_of(&root, json!({"path":"pipe"}))
            .await
            .unwrap_err();
        assert!(
            refused.to_string().contains("is not a regular file"),
            "{refused}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    async fn grep_in(root: &Path, args: Value, context: ToolContext) -> CoreResult<ToolOutput> {
        NativeToolRegistry::new(root.to_path_buf())
            .get("grep")
            .unwrap()
            .execute(args, context)
            .await
    }

    #[tokio::test]
    async fn grep_shows_the_head_of_a_long_matching_line() {
        let (root, _) = read_fixture(
            "bundle.min.js",
            format!("needle{}\nshort needle\n", "q".repeat(100_000)).as_bytes(),
        );
        let found = grep_in(&root, json!({"pattern":"needle"}), tool_context(&root))
            .await
            .unwrap();
        assert_eq!(
            found.text,
            format!(
                "bundle.min.js:1:needle{}{}\nbundle.min.js:2:short needle",
                "q".repeat(1_994),
                text::LINE_CUT
            )
        );
        assert!(found.metadata.get("outputPath").is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn grep_stops_at_its_match_limit_and_says_so() {
        let body = "hit\n".repeat(6_000);
        let (root, _) = read_fixture("a.txt", body.as_bytes());
        std::fs::write(root.join("b.txt"), &body).unwrap();
        std::fs::write(root.join("c.txt"), &body).unwrap();
        let found = grep_in(&root, json!({"pattern":"hit"}), tool_context(&root))
            .await
            .unwrap();
        // The listing is long enough to be saved whole; it holds the limit
        // exactly, however many files are left.
        let saved =
            std::fs::read_to_string(found.metadata["outputPath"].as_str().unwrap()).unwrap();
        assert_eq!(saved.lines().count(), 10_000);
        assert!(
            found
                .text
                .ends_with("(stopped at 10000 matching lines; narrow the pattern or the path)"),
            "{}",
            &found.text[found.text.len() - 200..]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn grep_leaves_the_async_workers_free_and_stop_ends_it() {
        let root = std::env::temp_dir().join(format!("haarness-grep-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let line = format!("{}\n", "w".repeat(200));
        for index in 0..4_000 {
            std::fs::write(root.join(format!("f{index}.txt")), line.repeat(20)).unwrap();
        }
        let context = tool_context(&root);
        let cancel = context.cancel.clone();
        // On this runtime's one thread, Stop can only come while the walk
        // leaves the thread free.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(2)).await;
            cancel.cancel();
        });
        let started = std::time::Instant::now();
        let stopped = grep_in(&root, json!({"pattern":"absent"}), context).await;
        assert!(matches!(stopped, Err(CoreError::Cancelled)), "{stopped:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
        std::fs::remove_dir_all(root).unwrap();
    }

    async fn edit_in(root: &Path, tool: &str, args: Value) -> CoreResult<ToolOutput> {
        NativeToolRegistry::new(root.to_path_buf())
            .get(tool)
            .unwrap()
            .execute(args, tool_context(root))
            .await
    }

    #[tokio::test]
    async fn an_edit_of_several_lines_meets_a_file_with_crlf_line_ends() {
        let (root, file) = read_fixture("main.c", b"int a() {\r\n    one();\r\n}\r\n");
        // What read shows, without the `\r`.
        edit_in(
            &root,
            "edit",
            json!({"path":"main.c","old":"    one();\n}","new":"    two();\n    three();\n}"}),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read(&file).unwrap(),
            b"int a() {\r\n    two();\r\n    three();\r\n}\r\n"
        );
        edit_in(
            &root,
            "multiedit",
            json!({"path":"main.c","edits":[
                {"old":"two();\n    three();","new":"four();"},
                {"old":"{\n","new":"{\n    // start\n","replaceAll":true}
            ]}),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read(&file).unwrap(),
            b"int a() {\r\n    // start\r\n    four();\r\n}\r\n"
        );
        // A one-line old with lines added: they end in CRLF too.
        edit_in(
            &root,
            "edit",
            json!({"path":"main.c","old":"    four();","new":"    four();\n    five();"}),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read(&file).unwrap(),
            b"int a() {\r\n    // start\r\n    four();\r\n    five();\r\n}\r\n"
        );
        // Lines added in a stretch of LF lines, or in a CSV cell that holds
        // bare LF, end in LF as their neighbours do.
        std::fs::write(&file, b"h1\r\nh2\r\nlf1\nlf2\nh3\r\nh4\r\n").unwrap();
        edit_in(
            &root,
            "edit",
            json!({"path":"main.c","old":"lf1","new":"lf1\nlf1b"}),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read(&file).unwrap(),
            b"h1\r\nh2\r\nlf1\nlf1b\nlf2\nh3\r\nh4\r\n"
        );
        std::fs::write(&file, b"id,note\r\n1,\"line a\nline b\"\r\n2,x\r\n3,y\r\n").unwrap();
        edit_in(
            &root,
            "edit",
            json!({"path":"main.c","old":"line a","new":"line A\nline A2"}),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read(&file).unwrap(),
            b"id,note\r\n1,\"line A\nline A2\nline b\"\r\n2,x\r\n3,y\r\n"
        );
        // Two places still refuse, as an LF file does.
        std::fs::write(&file, b"x\r\ny\r\nx\r\ny\r\n").unwrap();
        let ambiguous = edit_in(
            &root,
            "edit",
            json!({"path":"main.c","old":"x\ny","new":"z"}),
        )
        .await
        .unwrap_err();
        assert!(ambiguous.to_string().contains("found 2"), "{ambiguous}");
        // A file mostly of LF line ends is matched as written.
        std::fs::write(&file, b"a\nb\nc\r\nd\n").unwrap();
        let missed = edit_in(
            &root,
            "edit",
            json!({"path":"main.c","old":"c\nd","new":"z"}),
        )
        .await
        .unwrap_err();
        assert!(missed.to_string().contains("found 0"), "{missed}");
        edit_in(
            &root,
            "edit",
            json!({"path":"main.c","old":"a\nb","new":"ab"}),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"ab\nc\r\nd\n");
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod webfetch_tests {
    use super::*;
    use haarness_protocol::{ThreadId, TurnId};
    use tokio::io::AsyncReadExt as _;
    use tokio_util::sync::CancellationToken;

    fn composite_context(workspace: &Path) -> ToolContext {
        ToolContext {
            execution: haarness_core::ExecutionSettings::default(),
            thread_id: ThreadId::new(),
            turn_id: TurnId::new(),
            workspace: workspace.to_path_buf(),
            authorized_external_paths: Vec::new(),
            cancel: CancellationToken::new(),
            invoker: None,
        }
    }

    #[test]
    fn html_converts_to_readable_markdown() {
        let html = "<!doctype html><html><head><title>Page</title>\
                    <style>body { color: red }</style><script>headScript()</script></head>\
                    <body><h1>Title</h1>\
                    <p>Hello <strong>world</strong></p>\
                    <ul><li>one</li><li>two</li></ul>\
                    <script>ignore me</script><noscript>enable scripts</noscript>\
                    <p>after the script</p></body></html>";
        let markdown = html_to_markdown(html).unwrap();
        assert!(markdown.contains("# Title"), "{markdown}");
        assert!(markdown.contains("**world**"), "{markdown}");
        assert!(markdown.contains("one"), "{markdown}");
        assert!(markdown.contains("after the script"), "{markdown}");
        for hidden in ["ignore me", "headScript", "color: red", "enable scripts"] {
            assert!(!markdown.contains(hidden), "{hidden}: {markdown}");
        }
    }

    #[test]
    fn html_keeps_text_outside_any_element() {
        assert_eq!(
            html_to_markdown("bare-nonce-314159\n").as_deref(),
            Some("bare-nonce-314159")
        );
        let markdown =
            html_to_markdown("<p>kept</p><script>dropped()</script><style>p{}</style> tail")
                .unwrap();
        assert!(markdown.contains("kept"), "{markdown}");
        assert!(markdown.contains("tail"), "{markdown}");
        assert!(!markdown.contains("dropped"), "{markdown}");
        assert!(!markdown.contains("p{}"), "{markdown}");
    }

    #[tokio::test]
    async fn follows_redirects_and_identifies_haarness() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in [
                "HTTP/1.1 302 Found\r\nLocation: /article\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 50\r\nConnection: close\r\n\r\n<html><body><h1>Redirect worked</h1></body></html>",
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                loop {
                    let count = stream.read(&mut buffer).await.unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..count]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                requests.push(String::from_utf8(request).unwrap());
                stream.write_all(response.as_bytes()).await.unwrap();
            }
            requests
        });
        let workspace = std::env::current_dir().unwrap();
        let tool = BuiltinTool {
            kind: Kind::WebFetch,
            client: web_client(),
            skills: None,
            skill_catalog: Arc::from(""),
            instructions: Arc::new(InstructionDiscovery::default()),
        };
        let output = tool
            .execute(
                json!({"url": format!("http://{address}/start")}),
                ToolContext {
                    execution: haarness_core::ExecutionSettings::default(),
                    thread_id: ThreadId::new(),
                    turn_id: TurnId::new(),
                    workspace,
                    authorized_external_paths: Vec::new(),
                    cancel: CancellationToken::new(),
                    invoker: None,
                },
            )
            .await
            .unwrap();
        assert!(output.text.contains("# Redirect worked"), "{}", output.text);
        let requests = server.await.unwrap();
        assert!(requests[0].starts_with("GET /start HTTP/1.1"));
        assert!(requests[1].starts_with("GET /article HTTP/1.1"));
        for request in requests {
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("user-agent: haarness/"),
                "{request}"
            );
        }
    }

    /// Accepts one connection and reads its request head.
    async fn accept_request(listener: &tokio::net::TcpListener) -> (tokio::net::TcpStream, String) {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = stream.read(&mut buffer).await.unwrap();
            if count == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..count]);
        }
        (stream, String::from_utf8_lossy(&request).into_owned())
    }

    fn webfetch_tool() -> BuiltinTool {
        BuiltinTool {
            kind: Kind::WebFetch,
            client: web_client(),
            skills: None,
            skill_catalog: Arc::from(""),
            instructions: Arc::new(InstructionDiscovery::default()),
        }
    }

    #[test]
    fn redirects_are_followed_only_on_the_address_asked_for() {
        let url = |value: &str| url::Url::parse(value).unwrap();
        let host = Some("which is on another host");
        let port = Some("which is another port of the same host");
        for (from, to, leaves) in [
            ("http://a.example/x", "http://a.example/y", None),
            ("http://a.example/", "https://a.example/", None),
            ("https://a.example/", "https://A.EXAMPLE./login", None),
            ("http://a.example:8080/", "http://a.example:8080/y", None),
            (
                "https://a.example/",
                "http://a.example/",
                Some("which leaves https for unencrypted http"),
            ),
            ("http://a.example:8080/", "https://a.example/", port),
            ("https://a.example/", "https://a.example:8443/", port),
            ("https://a.example/", "https://b.example/", host),
            ("https://a.example/", "https://sub.a.example/", host),
        ] {
            assert_eq!(
                redirect_leaves(&url(from), &url(to)),
                leaves,
                "{from} -> {to}"
            );
        }
    }

    #[tokio::test]
    async fn a_redirect_to_another_port_says_so() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let elsewhere = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = format!("http://{}/page", elsewhere.local_addr().unwrap());
        let location = target.clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = accept_request(&listener).await;
            let head = format!(
                "HTTP/1.1 301 Moved Permanently\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(head.as_bytes()).await.unwrap();
        });
        let tool = webfetch_tool();
        let output = tool
            .execute(
                json!({"url": format!("http://{address}/start")}),
                composite_context(&std::env::current_dir().unwrap()),
            )
            .await
            .unwrap();
        assert!(
            output.text.contains(&format!(
                "to {target}, which is another port of the same host"
            )),
            "{}",
            output.text
        );
        server.await.unwrap();
        // The other port was never asked.
        assert!(
            tokio::time::timeout(Duration::from_millis(200), elsewhere.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn media_backends_are_still_followed_to_another_host() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, request) = accept_request(&listener).await;
            assert!(request.starts_with("POST /moved "), "{request}");
            let head = format!(
                "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://localhost:{}/generate\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                address.port()
            );
            stream.write_all(head.as_bytes()).await.unwrap();
            drop(stream);
            let (mut stream, request) = accept_request(&listener).await;
            assert!(request.starts_with("POST /generate "), "{request}");
            let body = r#"{"data":[{"b64_json":"iVBORw0KGgo="}],"mime_type":"image/png"}"#;
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).await.unwrap();
        });
        let root =
            std::env::temp_dir().join(format!("haarness-media-redirect-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let output = generate_media_from_endpoint(
            GeneratedMediaKind::Image,
            &json!({"prompt":"Quiet red fox"}),
            &root,
            media_client(),
            &CancellationToken::new(),
            MediaEndpoint {
                url: format!("http://{address}/moved"),
                api_key: None,
            },
        )
        .await
        .unwrap();
        assert!(output.metadata["path"].is_string(), "{}", output.metadata);
        server.await.unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_redirect_to_another_host_is_reported_and_not_followed() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let target = format!("http://localhost:{}/elsewhere", address.port());
        let location = target.clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = accept_request(&listener).await;
            let head = format!(
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(head.as_bytes()).await.unwrap();
            drop(stream);
            // A followed redirect would arrive here as a second connection.
            tokio::time::timeout(Duration::from_secs(2), listener.accept())
                .await
                .is_ok()
        });
        let tool = webfetch_tool();
        let output = tool
            .execute(
                json!({"url": format!("http://{address}/start")}),
                composite_context(&std::env::current_dir().unwrap()),
            )
            .await
            .unwrap();
        assert!(output.text.contains("another host"), "{}", output.text);
        assert!(
            output
                .text
                .contains(&format!("Call webfetch with {target}")),
            "{}",
            output.text
        );
        assert_eq!(output.metadata["redirect"], target);
        assert!(!server.await.unwrap(), "the redirect was followed");
    }

    const CHUNKED_HEAD: &[u8] =
        b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";

    fn chunk(size: usize) -> Vec<u8> {
        format!("{size:x}\r\n{}\r\n", "a".repeat(size)).into_bytes()
    }

    #[tokio::test]
    async fn stops_reading_a_body_without_length_once_it_passes_the_limit() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = accept_request(&listener).await;
            stream.write_all(CHUNKED_HEAD).await.unwrap();
            let chunk = chunk(64 * 1024);
            let mut sent = 0_usize;
            // Endless: only the client closing the connection ends it.
            while stream.write_all(&chunk).await.is_ok() {
                sent += 64 * 1024;
            }
            sent
        });
        let tool = webfetch_tool();
        let error = tool
            .execute(
                json!({"url": format!("http://{address}/endless")}),
                composite_context(&std::env::current_dir().unwrap()),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Response too large"), "{error}");
        let sent = tokio::time::timeout(Duration::from_secs(20), server)
            .await
            .expect("the fetch closed the connection")
            .unwrap();
        // Socket buffers hold some of what was sent past the limit.
        assert!(sent < WEBFETCH_MAX_BYTES + 16 * 1024 * 1024, "{sent}");
    }

    #[tokio::test]
    async fn refuses_a_declared_length_over_the_limit_without_reading_it() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = accept_request(&listener).await;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n",
                WEBFETCH_MAX_BYTES + 1
            );
            stream.write_all(head.as_bytes()).await.unwrap();
            tokio::time::sleep(Duration::from_secs(60)).await;
        });
        let tool = webfetch_tool();
        let error = tokio::time::timeout(
            Duration::from_secs(10),
            tool.execute(
                json!({"url": format!("http://{address}/large")}),
                composite_context(&std::env::current_dir().unwrap()),
            ),
        )
        .await
        .expect("refused before any body arrived")
        .unwrap_err();
        assert!(error.to_string().contains("Response too large"), "{error}");
        server.abort();
    }

    /// A server that sends its head and one chunk, then holds the
    /// connection open without finishing the body.
    async fn stalled_server() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = accept_request(&listener).await;
            stream.write_all(CHUNKED_HEAD).await.unwrap();
            stream.write_all(&chunk(16)).await.unwrap();
            tokio::time::sleep(Duration::from_secs(120)).await;
        });
        (address, server)
    }

    #[tokio::test]
    async fn stop_abandons_a_fetch_whose_body_never_finishes() {
        let (address, server) = stalled_server().await;
        let tool = webfetch_tool();
        let context = composite_context(&std::env::current_dir().unwrap());
        let cancel = context.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            cancel.cancel();
        });
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            tool.execute(json!({"url": format!("http://{address}/stall")}), context),
        )
        .await
        .expect("Stop ended the fetch");
        assert!(matches!(result, Err(CoreError::Cancelled)), "{result:?}");
        server.abort();
    }

    #[tokio::test]
    async fn a_fetch_whose_body_never_finishes_ends_at_its_deadline() {
        let (address, server) = stalled_server().await;
        let url = url::Url::parse(&format!("http://{address}/stall")).unwrap();
        let error = webfetch(
            &web_client(),
            url,
            false,
            &CancellationToken::new(),
            Duration::from_millis(300),
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("did not finish loading"),
            "{error}"
        );
        server.abort();
    }

    #[tokio::test]
    async fn a_limited_read_stops_at_its_limit() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, request) = accept_request(&listener).await;
                stream.write_all(CHUNKED_HEAD).await.unwrap();
                let chunks = if request.contains("/small") {
                    1
                } else {
                    usize::MAX
                };
                for _ in 0..chunks {
                    if stream.write_all(&chunk(1024)).await.is_err() {
                        break;
                    }
                }
                let _ = stream.write_all(b"0\r\n\r\n").await;
            }
        });
        let client = reqwest::Client::new();
        let small = client
            .get(format!("http://{address}/small"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            read_limited(small, 4096)
                .await
                .unwrap()
                .map(|body| body.len()),
            Some(1024)
        );
        let endless = client
            .get(format!("http://{address}/endless"))
            .send()
            .await
            .unwrap();
        assert_eq!(read_limited(endless, 4096).await.unwrap(), None);
        tokio::time::timeout(Duration::from_secs(20), server)
            .await
            .expect("the read closed the connection")
            .unwrap();
    }

    struct EchoInvoker;

    #[async_trait]
    impl haarness_core::ToolInvoker for EchoInvoker {
        async fn invoke(&self, name: &str, arguments: Value) -> CoreResult<ToolOutput> {
            if name == "fail" {
                return Err(CoreError::Tool("expected failure".into()));
            }
            Ok(ToolOutput {
                text: format!("{name}:ok"),
                metadata: arguments,
            })
        }

        fn side_effectful_for(&self, name: &str, _arguments: &Value) -> CoreResult<bool> {
            Ok(name == "write")
        }
    }

    #[tokio::test]
    async fn multiedit_is_ordered_and_atomic() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-multiedit-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::write(workspace.join("sample.txt"), "one two two\n")
            .await
            .unwrap();
        let registry = NativeToolRegistry::new(workspace.clone());
        registry
            .get("multiedit")
            .unwrap()
            .execute(
                json!({"path":"sample.txt","edits":[
                    {"old":"one","new":"first"},
                    {"old":"two","new":"second","replaceAll":true}
                ]}),
                composite_context(&workspace),
            )
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read_to_string(workspace.join("sample.txt"))
                .await
                .unwrap(),
            "first second second\n"
        );
        let failed = registry
            .get("multiedit")
            .unwrap()
            .execute(
                json!({"path":"sample.txt","edits":[
                    {"old":"first","new":"changed"},
                    {"old":"missing","new":"never"}
                ]}),
                composite_context(&workspace),
            )
            .await;
        assert!(failed.is_err());
        assert_eq!(
            tokio::fs::read_to_string(workspace.join("sample.txt"))
                .await
                .unwrap(),
            "first second second\n"
        );
        let _ = tokio::fs::remove_dir_all(workspace).await;
    }

    /// Counts how many invocations overlap, so a test can prove that batch
    /// really fans parallel-safe calls out instead of walking them.
    struct OverlapInvoker {
        active: std::sync::atomic::AtomicUsize,
        peak: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl haarness_core::ToolInvoker for OverlapInvoker {
        async fn invoke(&self, name: &str, arguments: Value) -> CoreResult<ToolOutput> {
            use std::sync::atomic::Ordering;
            let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(ToolOutput {
                text: format!("{name}:ok"),
                metadata: arguments,
            })
        }

        fn side_effectful_for(&self, name: &str, _arguments: &Value) -> CoreResult<bool> {
            Ok(name == "write")
        }

        fn parallel_safe(&self, name: &str, _arguments: &Value) -> bool {
            matches!(name, "task" | "read")
        }
    }

    #[tokio::test]
    async fn batch_fans_out_task_delegations_and_keeps_mutations_as_barriers() {
        let invoker = Arc::new(OverlapInvoker {
            active: std::sync::atomic::AtomicUsize::new(0),
            peak: std::sync::atomic::AtomicUsize::new(0),
        });
        let mut context = composite_context(Path::new("."));
        context.invoker = Some(invoker.clone());
        let output = BatchTool
            .execute(
                json!({"tool_calls":[
                    {"tool":"task","parameters":{"description":"audit a"}},
                    {"tool":"task","parameters":{"description":"audit b"}},
                    {"tool":"task","parameters":{"description":"audit c"}}
                ]}),
                context,
            )
            .await
            .unwrap();
        assert_eq!(output.metadata["failures"], 0);
        assert_eq!(
            invoker.peak.load(std::sync::atomic::Ordering::SeqCst),
            3,
            "three independent delegations must run side by side"
        );

        let invoker = Arc::new(OverlapInvoker {
            active: std::sync::atomic::AtomicUsize::new(0),
            peak: std::sync::atomic::AtomicUsize::new(0),
        });
        let mut context = composite_context(Path::new("."));
        context.invoker = Some(invoker.clone());
        BatchTool
            .execute(
                json!({"tool_calls":[
                    {"tool":"write","parameters":{"path":"a"}},
                    {"tool":"write","parameters":{"path":"b"}}
                ]}),
                context,
            )
            .await
            .unwrap();
        assert_eq!(
            invoker.peak.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "mutations stay ordered"
        );
        let registry = NativeToolRegistry::new(std::env::temp_dir());
        assert!(registry.get("task").unwrap().parallel_safe(&json!({})));
        assert!(!registry.get("write").unwrap().parallel_safe(&json!({})));
    }

    #[tokio::test]
    async fn batch_keeps_order_and_reports_individual_failures() {
        let mut context = composite_context(Path::new("."));
        context.invoker = Some(Arc::new(EchoInvoker));
        let output = BatchTool
            .execute(
                json!({"tool_calls":[
                    {"tool":"read","parameters":{"n":1}},
                    {"tool":"fail","parameters":{}},
                    {"tool":"grep","parameters":{"n":3}}
                ]}),
                context,
            )
            .await
            .unwrap();
        assert_eq!(output.metadata["failures"], 1);
        assert_eq!(output.metadata["results"][0]["tool"], "read");
        assert_eq!(
            output.metadata["results"][1]["error"],
            "tool error: expected failure"
        );
        assert_eq!(output.metadata["results"][2]["metadata"]["n"], 3);
    }

    #[tokio::test]
    async fn code_mode_calls_only_mcp_tools_in_confined_runtime() {
        if !WorkflowExecutor::default().capability().executable {
            eprintln!("Bun unavailable; skipping portable code-mode execution test");
            return;
        }
        let mut context = composite_context(Path::new("."));
        context.invoker = Some(Arc::new(EchoInvoker));
        let output = ExecuteTool
            .execute(
                json!({"code":"return (await tool('mcp__demo__echo', { value: 42 })).metadata"}),
                context.clone(),
            )
            .await
            .unwrap();
        assert_eq!(output.metadata["result"]["value"], 42);

        let refused = ExecuteTool
            .execute(
                json!({"code":"return await tool('read', { path: 'secret' })"}),
                context,
            )
            .await
            .unwrap_err();
        assert!(refused.to_string().contains("mcp__-prefixed"));
    }
    #[tokio::test]
    async fn parity_media_and_code_budgets_can_be_relaxed_without_bypassing_tool_boundaries() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-budget-parity-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&workspace).unwrap();
        let file = std::fs::File::create(workspace.join("large.pdf")).unwrap();
        file.set_len(11 * 1024 * 1024).unwrap();
        let registry = NativeToolRegistry::new(workspace.clone());
        let mut context = composite_context(&workspace);
        assert!(
            registry
                .get("read")
                .unwrap()
                .execute(json!({"path":"large.pdf"}), context.clone())
                .await
                .is_ok()
        );
        context.execution.media_max_bytes = 10 * 1024 * 1024;
        assert!(
            registry
                .get("read")
                .unwrap()
                .execute(json!({"path":"large.pdf"}), context.clone())
                .await
                .unwrap_err()
                .to_string()
                .contains("inline media limit")
        );
        context.invoker = Some(Arc::new(EchoInvoker));
        context.execution.code_max_calls = 1;
        let code = "await tool('mcp__demo__echo', {}); return await tool('mcp__demo__echo', {})";
        assert!(
            ExecuteTool
                .execute(json!({"code":code}), context.clone())
                .await
                .unwrap_err()
                .to_string()
                .contains("exceeded 1 tool calls")
        );
        context.execution.code_max_calls = 0;
        context.execution.code_timeout_ms = 0;
        assert!(
            ExecuteTool
                .execute(json!({"code":code}), context.clone())
                .await
                .is_ok()
        );
        context.execution.code_max_source_bytes = 10;
        assert!(
            ExecuteTool
                .execute(json!({"code":code}), context)
                .await
                .is_err()
        );
        std::fs::remove_dir_all(workspace).unwrap();
    }

    /// An MCP server that accepts a call and never answers.
    struct HungMcp;

    #[async_trait]
    impl McpRegistry for HungMcp {
        fn tool_snapshot(&self) -> Vec<ToolDefinition> {
            Vec::new()
        }
        async fn list(&self) -> CoreResult<Vec<haarness_protocol::McpServerInfo>> {
            Ok(Vec::new())
        }
        async fn configure(
            &self,
            _name: &str,
            _config: haarness_protocol::McpConfigureRequest,
        ) -> CoreResult<haarness_protocol::McpServerInfo> {
            unreachable!()
        }
        async fn reconnect(&self, _name: &str) -> CoreResult<haarness_protocol::McpServerInfo> {
            unreachable!()
        }
        async fn disconnect(&self, _name: &str) -> CoreResult<haarness_protocol::McpServerInfo> {
            unreachable!()
        }
        async fn tools(&self, _name: &str) -> CoreResult<Vec<ToolDefinition>> {
            Ok(Vec::new())
        }
        async fn call(
            &self,
            _name: &str,
            _tool: &str,
            _arguments: Value,
        ) -> CoreResult<ToolOutput> {
            std::future::pending().await
        }
        async fn resources(&self, _name: &str) -> CoreResult<Vec<Value>> {
            Ok(Vec::new())
        }
        async fn resource_templates(&self, _name: &str) -> CoreResult<Vec<Value>> {
            Ok(Vec::new())
        }
        async fn read_resource(&self, _name: &str, _uri: &str) -> CoreResult<Value> {
            unreachable!()
        }
        async fn prompts(&self, _name: &str) -> CoreResult<Vec<Value>> {
            Ok(Vec::new())
        }
        async fn get_prompt(
            &self,
            _name: &str,
            _prompt: &str,
            _arguments: Value,
        ) -> CoreResult<Value> {
            unreachable!()
        }
    }

    #[tokio::test]
    async fn stop_abandons_an_mcp_call_that_never_answers() {
        let tool = McpProjectedTool {
            definition: ToolDefinition {
                name: "slow__wait".into(),
                description: "waits".into(),
                input_schema: json!({"type":"object"}),
            },
            server: "slow".into(),
            registry: Arc::new(HungMcp),
        };
        let context = composite_context(&std::env::temp_dir());
        let cancel = context.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            cancel.cancel();
        });
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tool.execute(json!({}), context),
        )
        .await
        .expect("Stop must end the call");
        assert!(matches!(result, Err(CoreError::Cancelled)), "{result:?}");
    }
}

/// What reaches the agent's shell from the skill tool and Code Mode: a
/// skill's `!` block is marked as an agent's command, and Code Mode cannot
/// run a shell command at all.
#[cfg(all(test, unix))]
mod agent_shell_tests {
    use super::*;
    use futures::stream;
    use haarness_core::{
        AgentRuntime, EventBus, ModelProvider, PermissionDecision, PermissionRule, ProviderEvent,
        ProviderMessage, ProviderRequest, ProviderStream, RuntimeConfig, StaticProviderRegistry,
    };
    use haarness_protocol::{
        CreateThreadRequest, ItemData, ProviderInfo, RequestId, StartTurnRequest, TurnStatus,
    };
    use haarness_store::SqliteStore;
    use tokio_util::sync::CancellationToken;

    /// Loads the `marks` skill on the first round, then answers.
    struct LoadsSkill;

    #[async_trait]
    impl ModelProvider for LoadsSkill {
        fn id(&self) -> &str {
            "fixture"
        }
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                id: "fixture".into(),
                name: "Skill fixture".into(),
                authenticated: true,
                models: vec![],
            }
        }
        async fn stream(
            &self,
            request: ProviderRequest,
            _cancel: CancellationToken,
        ) -> CoreResult<ProviderStream> {
            let answered = request
                .messages
                .iter()
                .any(|message| matches!(message, ProviderMessage::ToolResult { .. }));
            let event = if answered {
                ProviderEvent::TextDelta("done".into())
            } else {
                ProviderEvent::ToolCall {
                    call_id: "skill-call".into(),
                    name: "skill".into(),
                    arguments: json!({"name": "marks"}),
                    provider_metadata: Value::Null,
                }
            };
            Ok(Box::pin(stream::iter([
                Ok(event),
                Ok(ProviderEvent::Finished {
                    reason: "stop".into(),
                }),
            ])))
        }
    }

    #[tokio::test]
    async fn a_skill_s_block_is_marked_as_an_agent_s_command_and_names_the_chat() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-skill-markers-{}", uuid::Uuid::now_v7()));
        let skills_root = workspace.join("skills");
        tokio::fs::create_dir_all(skills_root.join("marks"))
            .await
            .unwrap();
        tokio::fs::write(
            skills_root.join("marks/SKILL.md"),
            "---\ndescription: Shows the agent markers\n---\nMarks: !`printf '%s|%s|%s|%s' \"${AGENT-unset}\" \"${HAARNESS-unset}\" \"${AI_AGENT-unset}\" \"${HAARNESS_SESSION_ID-unset}\"`\n",
        )
        .await
        .unwrap();
        let registry = crate::FilesystemSkillRegistry::new(vec![(skills_root, true)]);
        let infos = registry.discover().await.unwrap();
        let tools = NativeToolRegistry::with_extras(
            workspace.clone(),
            None,
            Some(Arc::new(registry)),
            infos,
        );
        let mut config = RuntimeConfig::default();
        for pattern in ["bash:*", "skill:*"] {
            config.base_rules.push(PermissionRule {
                pattern: pattern.into(),
                decision: PermissionDecision::Allow,
            });
        }
        let runtime = AgentRuntime::with_config(
            Arc::new(SqliteStore::in_memory().unwrap()),
            Arc::new(StaticProviderRegistry::new([
                Arc::new(LoadsSkill) as Arc<dyn ModelProvider>
            ])),
            Arc::new(tools),
            EventBus::new(256),
            workspace.clone(),
            String::new(),
            config,
        );
        let thread = runtime
            .create_thread(CreateThreadRequest {
                title: Some("Skill markers".into()),
                provider: Some("fixture".into()),
                model: Some("fixture".into()),
                ..CreateThreadRequest::default()
            })
            .await
            .unwrap();
        let turn = runtime
            .start_turn(
                thread.id,
                StartTurnRequest {
                    request_id: RequestId::new(),
                    text: "load the skill".into(),
                    attachments: vec![],
                    synthetic: false,
                    auto_approve: false,
                    agent: None,
                    provider: None,
                    model: None,
                    reasoning_effort: None,
                    tools: None,
                    command: None,
                    resources: Vec::new(),
                },
            )
            .await
            .unwrap();
        let status = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let status = runtime.turn(turn.id).await.unwrap().status;
                if !matches!(status, TurnStatus::Queued | TurnStatus::Running) {
                    return status;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the turn settles");
        assert_eq!(status, TurnStatus::Completed);
        // An `AI_AGENT` this test process has names its outer agent and is kept.
        let ai_agent = std::env::var("AI_AGENT")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| haarness_core::AI_AGENT_VALUE.into());
        let expected = format!("Marks: 1|1|{ai_agent}|{}", thread.id);
        let items = runtime.items(thread.id, None).await.unwrap();
        assert!(
            items.iter().any(|item| matches!(&item.data,
                ItemData::ToolResult { name, output, is_error: false, .. }
                    if name == "skill" && output.contains(&expected))),
            "no skill result carried {expected}: {items:?}"
        );
        let _ = tokio::fs::remove_dir_all(workspace).await;
    }

    /// Fails the test if Code Mode reaches the agent's tools at all.
    struct NoTools;

    #[async_trait]
    impl haarness_core::ToolInvoker for NoTools {
        async fn invoke(&self, name: &str, _arguments: Value) -> CoreResult<ToolOutput> {
            panic!("code mode invoked {name}");
        }
        fn side_effectful_for(&self, _name: &str, _arguments: &Value) -> CoreResult<bool> {
            Ok(true)
        }
    }

    /// Code Mode's `execute` calls MCP and browser tools only: neither its
    /// `shell()` nor `tool("bash")` reaches a shell, so it has no command to
    /// mark.
    #[tokio::test]
    async fn code_mode_cannot_run_a_shell_command() {
        let host = McpCodeHost {
            invoker: Arc::new(NoTools),
            calls: std::sync::atomic::AtomicUsize::new(0),
            max_calls: 100,
        };
        let call = |method, params| HostCall {
            run_id: "code".into(),
            request_id: 1,
            method,
            params,
            cancellation: CancellationToken::new(),
        };
        let shell = host
            .request(call(HostRequestMethod::Shell, json!({"command": "env"})))
            .await
            .unwrap_err();
        assert!(
            shell
                .to_string()
                .contains("MCP and browser tool calls only"),
            "{shell}"
        );
        let bash = host
            .request(call(
                HostRequestMethod::Tool,
                json!({"name": "bash", "args": {"command": "env"}}),
            ))
            .await
            .unwrap_err();
        assert!(bash.to_string().contains("only mcp__-prefixed"), "{bash}");
    }
}
