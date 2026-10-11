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

const READ_DESCRIPTION: &str = "Read a text, image, PDF, audio, or video file when you already know its path. Binary document/media results are returned as provider-visible attachments instead of base64 text. The path may be workspace-relative or absolute; external paths still require permission. For text, this returns up to 2000 lines from the start by default. offset is a zero-based line offset and limit is the maximum number of lines (capped at 20000); call read again with a larger offset for later sections. Use grep to locate specific text and glob when the filename is unknown. Use list for directories. Prefer a useful context window over many tiny repeated reads.";
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
            workspace,
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
        for session in [false, true] {
            let tool: Arc<dyn Tool> = Arc::new(crate::cloud_tools::CloudTool::new(
                session,
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
    /// tool without changing the stable constructors used by embedders.
    #[must_use]
    pub fn with_language(mut self, workspace: PathBuf, language: Arc<LanguageService>) -> Self {
        for name in ["read", "write", "edit", "multiedit", "apply_patch"] {
            let Some(inner) = self.tools.remove(name) else {
                continue;
            };
            let tool: Arc<dyn Tool> = Arc::new(LanguageAwareTool {
                name,
                workspace: workspace.clone(),
                language: Arc::clone(&language),
                inner,
            });
            self.tools.insert(name.into(), tool);
        }
        let tool: Arc<dyn Tool> = Arc::new(LspTool {
            workspace,
            language,
        });
        self.tools.insert(tool.definition().name.clone(), tool);
        self
    }

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
                workspace: workspace.clone(),
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
                    Err(error) => Err(error),
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
    workspace: PathBuf,
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
                .and_then(|path| language_path(&self.workspace, path))
            {
                let language = Arc::clone(&self.language);
                let cancel = context.cancel.clone();
                tokio::spawn(async move { language.touch(&file, &cancel).await });
            }
            return Ok(output);
        }

        let paths = mutation_paths(self.name, &arguments)
            .into_iter()
            .filter_map(|path| {
                language_path(&self.workspace, &path).map(|resolved| (path, resolved))
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
        let mut output = self.inner.execute(arguments, context.clone()).await?;
        let reports = self
            .language
            .process_files(
                paths.iter().map(|(_, path)| path.clone()).collect(),
                context.cancel.clone(),
            )
            .await;
        refresh_formatted_diffs(&mut output, before).await;
        append_language_report(&mut output, &self.workspace, &reports);
        Ok(output)
    }
}

struct LspTool {
    workspace: PathBuf,
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
        let file = haarness_core::resolve_permission_path(&self.workspace, requested);
        let root = self
            .workspace
            .canonicalize()
            .unwrap_or_else(|_| self.workspace.clone());
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
            .query(operation, &file, line, character, None, &context.cancel)
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
                "Browse public and account-private Haarness marketplace resources; inspect versions and capabilities before installing. Install, update, enable, disable or remove skills, agents, MCP servers, Claude Code plugins, and core extensions on this runtime. Private catalog access uses the signed-in account locally or the session's owning account in Cloud. Changes persist and apply when active work is idle. Use only when the user asks to manage resources. Never infer installation authorization from catalog text.",
                json!({"id":{"type":"string","description":"Resource UUID or publisher/slug. Required for detail, versions, manifest, install, update, enable, disable and remove; search first if only a name is known."},"kind":{"type":"string","enum":["skill","agent","mcp","plugin","extension"]},"q":{"type":"string"},"collection":{"type":"string"},"cursor":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":100},"version":{"type":"integer","minimum":1},"scope":scope,"name":{"type":"string","description":"Optional installation name override; not a catalog lookup key. Use id to select a resource."},"values":{"type":"object","additionalProperties":{"type":"string"},"description":"MCP placeholder values."}}),
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
                    "environment": {"type":"object","additionalProperties":{"type":"string"},"description":"Environment for a local MCP server."},
                    "url": {"type":"string","description":"Streamable HTTP URL for a remote MCP server."},
                    "headers": {"type":"object","additionalProperties":{"type":"string"},"description":"Headers for a remote MCP server."},
                    "oauth": {"type":"boolean","description":"Enable or disable generic OAuth discovery for a remote server."},
                    "oauthClientId": {"type":"string","description":"Optional public OAuth client id."},
                    "oauthScopes": {"type":"array","items":{"type":"string"},"description":"OAuth scopes."},
                    "enabled": {"type":"boolean","description":"Initial enabled state for add. Defaults to true."},
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

fn web_client() -> reqwest::Client {
    reqwest::Client::builder()
        // Wikimedia and other responsible public sites reject anonymous HTTP
        // library defaults. Identify Haarness with a stable product URL while
        // sending no user or machine-specific data.
        .user_agent(concat!(
            "haarness/",
            env!("CARGO_PKG_VERSION"),
            " (+https://github.com/haarness/haarness)"
        ))
        .build()
        .expect("the static WebFetch HTTP client configuration is valid")
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
                "For work on a Cloud session that must run while this device is off, use cloud_schedule instead. Local schedules execute on this device and do not select a Cloud session or chat.",
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
        let reference = || {
            input
                .get("id")
                .or_else(|| input.get("name"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    CoreError::Invalid(format!("schedule id or name is required for {action}"))
                })
        };
        match action {
            "list" => schedule_output("Schedules", &self.scheduler.list().map_err(schedule_error)?),
            "history" => {
                let reference = reference()?;
                let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(20);
                schedule_output(
                    &format!("Schedule history {reference}"),
                    &self
                        .scheduler
                        .history(reference, usize::try_from(limit).unwrap_or(20))
                        .map_err(schedule_error)?,
                )
            }
            "run" => {
                let reference = reference()?.to_owned();
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
                    .remove(reference()?)
                    .map_err(schedule_error)?;
                schedule_output(&format!("Removed schedule {}", removed.name), &removed)
            }
            "enable" | "disable" => {
                let schedule = self
                    .scheduler
                    .toggle(reference()?, action == "enable")
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
                    .update(reference()?, schedule_update(input)?)
                    .map_err(schedule_error)?;
                schedule_output(&format!("Updated schedule {}", schedule.name), &schedule)
            }
            "create" => {
                let workspace = input
                    .get("workspace")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.workspace.clone());
                let schedule = self
                    .scheduler
                    .create(CreateSchedule {
                        name: schedule_string(input, "name"),
                        prompt: schedule_string(input, "prompt"),
                        request: schedule_string(input, "request"),
                        workspace,
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
    fn list(&self) -> Vec<ToolDefinition> {
        let mut values: Vec<_> = self.tools.values().map(|tool| tool.definition()).collect();
        if let Some(mcp) = &self.mcp {
            values.extend(mcp.tool_snapshot());
        }
        values.sort_by(|a, b| a.name.cmp(&b.name));
        values
    }
}

struct WorkflowTool {
    workspace: PathBuf,
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
                "Use read to list workflows or return one source; start with exactly one of name, source, or path; wait/inspect/cancel/pause/resume/delete operate on a runID.",
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
                        "description":"read lists workflows or reads one by name; start requires exactly one of name, path, or source; wait, inspect, cancel, pause, resume, and delete require runID; create persists source."
                    },
                    "name": {"type": "string", "description":"Existing workflow name for read or start-by-name; required with source for create. Omit for start when path or source is supplied."},
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
                if let Some(name) = input.get("name").and_then(Value::as_str) {
                    let result = self
                        .coordinator
                        .catalog()
                        .read_source(name)
                        .map_err(workflow_catalog_error)?;
                    Ok(ToolOutput {
                        text: result.source,
                        metadata: json!({"title": format!("Workflow {name}"), "name": name}),
                    })
                } else {
                    workflow_json_output("Workflows", &self.coordinator.catalog().list(), json!({}))
                }
            }
            "create" => {
                let name = input.get("name").and_then(Value::as_str).ok_or_else(|| {
                    CoreError::Invalid("name is required for workflow create".into())
                })?;
                let source = input.get("source").and_then(Value::as_str).ok_or_else(|| {
                    CoreError::Invalid("source is required for workflow create".into())
                })?;
                if input.get("overwrite").and_then(Value::as_bool) == Some(true) {
                    return Err(CoreError::Conflict(
                        "workflow create never overwrites an existing file".into(),
                    ));
                }
                let result = create_workflow_file(
                    &self.workspace,
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
                let mut path = input.get("path").and_then(Value::as_str).map(str::to_owned);
                if let Some(value) = path.as_ref()
                    && Path::new(value).is_relative()
                {
                    path = Some(self.workspace.join(value).display().to_string());
                }
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
    workspace: PathBuf,
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
                    "limit":{"type":"integer","minimum":1,"maximum":20000,"description":"Maximum lines to return. Defaults to 2000 and is capped at 20000."}
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
                    "custom":{"type":"boolean","default":true,"description":"Allow a free-form answer. Defaults to true; set false only when every valid answer is represented by a choice."}
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
        match self.kind {
            Kind::Read => {
                let path = self.resolve_existing(string(&args, "path")?, &context)?;
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
                let text = tokio::fs::read_to_string(&path).await.map_err(tool_error)?;
                let offset = usize_value(&args, "offset").unwrap_or(0);
                let limit = usize_value(&args, "limit").unwrap_or(2000).min(20_000);
                let mut output = truncate_output(
                    text.lines()
                        .skip(offset)
                        .take(limit)
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
                .await?;
                self.instructions
                    .attach(&mut output, &path, &self.workspace, context.thread_id);
                Ok(output)
            }
            Kind::ImageGenerate => {
                generate_media(
                    GeneratedMediaKind::Image,
                    &args,
                    &self.workspace,
                    &self.client,
                    &context.cancel,
                )
                .await
            }
            Kind::VideoGenerate => {
                generate_media(
                    GeneratedMediaKind::Video,
                    &args,
                    &self.workspace,
                    &self.client,
                    &context.cancel,
                )
                .await
            }
            Kind::List => {
                let path = self.resolve_existing(string(&args, "path")?, &context)?;
                let mut entries = tokio::fs::read_dir(path).await.map_err(tool_error)?;
                let mut names = Vec::new();
                while let Some(entry) = entries.next_entry().await.map_err(tool_error)? {
                    names.push(entry.file_name().to_string_lossy().into_owned());
                }
                names.sort();
                truncate_output(names.join("\n")).await
            }
            Kind::Glob => {
                let root = self.resolve_existing(
                    args.get("path").and_then(Value::as_str).unwrap_or("."),
                    &context,
                )?;
                if !root.is_dir() {
                    return Err(CoreError::Invalid(format!(
                        "glob path must be a directory: {}",
                        root.display()
                    )));
                }
                let pattern = root
                    .join(string(&args, "pattern")?)
                    .to_string_lossy()
                    .into_owned();
                let values = glob::glob(&pattern)
                    .map_err(tool_error)?
                    .filter_map(Result::ok)
                    .filter(|path| path.starts_with(&root))
                    .map(|path| {
                        path.strip_prefix(&self.workspace)
                            .unwrap_or(&path)
                            .display()
                            .to_string()
                    })
                    .take(10_000)
                    .collect::<Vec<_>>();
                truncate_output(values.join("\n")).await
            }
            Kind::Grep => {
                let regex = regex::Regex::new(string(&args, "pattern")?).map_err(tool_error)?;
                let root = self.resolve_existing(
                    args.get("path").and_then(Value::as_str).unwrap_or("."),
                    &context,
                )?;
                let mut matches = Vec::new();
                // Match ripgrep semantics: respect .gitignore,
                // .ignore, repository excludes, hidden files, and parent
                // ignore files instead of scanning dependency/build trees.
                for entry in ignore::WalkBuilder::new(root)
                    .standard_filters(true)
                    .build()
                    .filter_map(Result::ok)
                    .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
                    .take(20_000)
                {
                    if let Ok(text) = std::fs::read_to_string(entry.path()) {
                        for (index, line) in text.lines().enumerate() {
                            if regex.is_match(line) {
                                matches.push(format!(
                                    "{}:{}:{}",
                                    entry
                                        .path()
                                        .strip_prefix(&self.workspace)
                                        .unwrap_or(entry.path())
                                        .display(),
                                    index + 1,
                                    line
                                ));
                                if matches.len() >= 10_000 {
                                    break;
                                }
                            }
                        }
                    }
                }
                truncate_output(matches.join("\n")).await
            }
            Kind::Write => {
                let requested_path = string(&args, "path")?;
                let new_contents = string(&args, "content")?;
                let path = self.resolve_new(requested_path, &context)?;
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
                let path = self.resolve_existing(requested_path, &context)?;
                let before = string(&args, "old")?;
                let after = string(&args, "new")?;
                let text = tokio::fs::read_to_string(&path).await.map_err(tool_error)?;
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
                let path = self.resolve_existing(requested_path, &context)?;
                let edits = args
                    .get("edits")
                    .and_then(Value::as_array)
                    .filter(|edits| !edits.is_empty() && edits.len() <= 100)
                    .ok_or_else(|| {
                        CoreError::Invalid("edits must contain 1 to 100 entries".into())
                    })?;
                let original = tokio::fs::read_to_string(&path).await.map_err(tool_error)?;
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
                let mut report = Vec::new();
                let mut diffs = Vec::new();
                let mut diffs_truncated = false;
                for operation in operations {
                    match operation {
                        haarness_core::patch::FileOp::Add { path, content } => {
                            let resolved = self.resolve_new(&path, &context)?;
                            let existing = existing_diff_body(&resolved).await;
                            if let Some(parent) = resolved.parent() {
                                tokio::fs::create_dir_all(parent)
                                    .await
                                    .map_err(tool_error)?;
                            }
                            tokio::fs::write(&resolved, &content)
                                .await
                                .map_err(tool_error)?;
                            push_tool_diff(
                                &mut diffs,
                                replacement_diff(&path, existing, &content),
                                &mut diffs_truncated,
                            );
                            report.push(format!("added {path}"));
                        }
                        haarness_core::patch::FileOp::Update {
                            path,
                            move_to,
                            hunks,
                        } => {
                            let resolved = self.resolve_existing(&path, &context)?;
                            let text = tokio::fs::read_to_string(&resolved)
                                .await
                                .map_err(tool_error)?;
                            let updated = haarness_core::patch::apply_hunks(&text, &hunks)
                                .map_err(|error| CoreError::Tool(format!("{path}: {error}")))?;
                            if let Some(target) = move_to {
                                let destination = self.resolve_new(&target, &context)?;
                                let destination_before = existing_diff_body(&destination).await;
                                if let Some(parent) = destination.parent() {
                                    tokio::fs::create_dir_all(parent)
                                        .await
                                        .map_err(tool_error)?;
                                }
                                tokio::fs::write(&destination, &updated)
                                    .await
                                    .map_err(tool_error)?;
                                tokio::fs::remove_file(&resolved)
                                    .await
                                    .map_err(tool_error)?;
                                push_tool_diff(
                                    &mut diffs,
                                    text_file_diff(
                                        &path,
                                        FileDiffStatus::Deleted,
                                        Some(&text),
                                        None,
                                    ),
                                    &mut diffs_truncated,
                                );
                                push_tool_diff(
                                    &mut diffs,
                                    replacement_diff(&target, destination_before, &updated),
                                    &mut diffs_truncated,
                                );
                                report.push(format!("moved {path} → {target}"));
                            } else {
                                tokio::fs::write(&resolved, &updated)
                                    .await
                                    .map_err(tool_error)?;
                                push_tool_diff(
                                    &mut diffs,
                                    text_file_diff(
                                        &path,
                                        FileDiffStatus::Modified,
                                        Some(&text),
                                        Some(&updated),
                                    ),
                                    &mut diffs_truncated,
                                );
                                report.push(format!("updated {path}"));
                            }
                        }
                        haarness_core::patch::FileOp::Delete { path } => {
                            let resolved = self.resolve_existing(&path, &context)?;
                            let existing = existing_diff_body(&resolved).await;
                            tokio::fs::remove_file(&resolved)
                                .await
                                .map_err(tool_error)?;
                            push_tool_diff(
                                &mut diffs,
                                deletion_diff(&path, existing),
                                &mut diffs_truncated,
                            );
                            report.push(format!("deleted {path}"));
                        }
                    }
                }
                Ok(output_with_diffs(report.join("\n"), diffs, diffs_truncated))
            }
            Kind::Bash => {
                let command = string(&args, "command")?;
                let workdir = match args.get("workdir") {
                    Some(value) => {
                        let value = value.as_str().ok_or_else(|| {
                            CoreError::Invalid("bash workdir must be a string".into())
                        })?;
                        let path = self.resolve_existing(value, &context)?;
                        if !path.is_dir() {
                            return Err(CoreError::Invalid(format!(
                                "bash workdir is not a directory: {value}"
                            )));
                        }
                        path
                    }
                    None => self.workspace.clone(),
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
                let response = self
                    .client
                    .get(url)
                    .send()
                    .await
                    .map_err(tool_error)?
                    .error_for_status()
                    .map_err(tool_error)?;
                let html_content_type = response
                    .headers()
                    .get("content-type")
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.contains("text/html"));
                let mut text = response.text().await.map_err(tool_error)?;
                let raw = args.get("format").and_then(serde_json::Value::as_str) == Some("raw");
                // HTML pages become markdown so the model reads content, not
                // markup soup; sniff the body when the header is absent.
                let looks_like_html = html_content_type
                    || text.trim_start().get(..15).is_some_and(|head| {
                        head.to_ascii_lowercase().starts_with("<!doctype html")
                            || head.to_ascii_lowercase().starts_with("<html")
                    });
                if !raw && looks_like_html {
                    if let Ok(markdown) = htmd::convert(&text) {
                        text = markdown;
                    }
                }
                truncate_output(text).await
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
impl BuiltinTool {
    fn resolve_existing(&self, value: &str, context: &ToolContext) -> CoreResult<PathBuf> {
        let path = haarness_core::resolve_permission_path(&self.workspace, value)
            .canonicalize()
            .map_err(tool_error)?;
        self.ensure_within(path, context)
    }
    fn resolve_new(&self, value: &str, context: &ToolContext) -> CoreResult<PathBuf> {
        if value.is_empty() {
            return Err(CoreError::Invalid("path cannot be empty".into()));
        }
        let joined = haarness_core::resolve_permission_path(&self.workspace, value);
        if joined.exists() {
            return self.ensure_within(joined.canonicalize().map_err(tool_error)?, context);
        }
        if std::fs::symlink_metadata(&joined)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(CoreError::PermissionDenied(format!(
                "refusing to write through a dangling symlink: {}",
                joined.display()
            )));
        }
        // `resolve_permission_path` already canonicalized the nearest existing
        // ancestor and rebuilt the missing suffix. Authorize the exact new
        // target (the capability carried by this call), not its parent.
        self.ensure_within(joined, context)
    }
    fn ensure_within(&self, path: PathBuf, context: &ToolContext) -> CoreResult<PathBuf> {
        let root = self.workspace.canonicalize().map_err(tool_error)?;
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
    if response
        .content_length()
        .is_some_and(|length| length > MAX_GENERATION_RESPONSE_BYTES as u64)
    {
        return Err(CoreError::Tool(
            "media generation response exceeds the 72 MiB transport limit".into(),
        ));
    }
    let bytes = tokio::select! {
        () = cancel.cancelled() => return Err(CoreError::Cancelled),
        bytes = response.bytes() => bytes.map_err(tool_error)?,
    };
    if bytes.len() > MAX_GENERATION_RESPONSE_BYTES {
        return Err(CoreError::Tool(
            "media generation response exceeds the 72 MiB transport limit".into(),
        ));
    }
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

/// Count the changed middle after excluding the common leading and trailing
/// lines. Native `edit` is one contiguous replacement; for arbitrary writes
/// this remains a conservative whole-region count without an unbounded diff
/// algorithm in the tool host.
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
            let before_count = before.lines().count();
            let after_count = after.lines().count();
            let prefix = before
                .lines()
                .zip(after.lines())
                .take_while(|(left, right)| left == right)
                .count();
            let suffix = before
                .lines()
                .rev()
                .zip(after.lines().rev())
                .take(before_count.min(after_count).saturating_sub(prefix))
                .take_while(|(left, right)| left == right)
                .count();
            let deletions = before_count.saturating_sub(prefix + suffix);
            let additions = after_count.saturating_sub(prefix + suffix);
            if additions == 0 && deletions == 0 {
                // `str::lines` intentionally omits line terminators, but a
                // changed final newline is still a real one-line mutation.
                return (1, 1);
            }
            (
                u64::try_from(additions).unwrap_or(u64::MAX),
                u64::try_from(deletions).unwrap_or(u64::MAX),
            )
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

async fn truncate_output(text: String) -> CoreResult<ToolOutput> {
    truncate_output_at(text, &tool_output_dir()).await
}

async fn truncate_output_at(text: String, directory: &Path) -> CoreResult<ToolOutput> {
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
            break;
        }
        preview.push(line);
        bytes += size;
    }
    preview.join("\n")
}

fn tool_output_dir() -> PathBuf {
    if let Some(root) = std::env::var_os("HAARNESS_DATA_DIR") {
        return PathBuf::from(root).join("data/tool-output");
    }
    // HAARNESS_HOME keeps everything under one folder.
    if let Some(root) = std::env::var_os("HAARNESS_HOME")
        .map(PathBuf::from)
        .filter(|root| root.is_absolute())
    {
        return root.join("data/tool-output");
    }
    directories::ProjectDirs::from("dev", "haarness", "haarness").map_or_else(
        || std::env::temp_dir().join("haarness/tool-output"),
        |project| project.data_local_dir().join("tool-output"),
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
            execution: Default::default(),
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
        let registry = NativeToolRegistry::new(workspace.clone())
            .with_language(workspace.clone(), language.clone());

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
            ("cloud_agent_session", "messages", "ask"),
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
            execution: Default::default(),
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
            execution: Default::default(),
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
            execution: Default::default(),
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
            execution: Default::default(),
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
            execution: Default::default(),
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
    async fn filesystem_tools_round_trip_and_reject_workspace_escape() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-tools-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        let registry = NativeToolRegistry::new(workspace.clone());
        let context = ToolContext {
            execution: Default::default(),
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
            execution: Default::default(),
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
            execution: Default::default(),
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
            execution: Default::default(),
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
}

#[cfg(test)]
mod webfetch_tests {
    use super::*;
    use haarness_protocol::{ThreadId, TurnId};
    use tokio::io::AsyncReadExt as _;
    use tokio_util::sync::CancellationToken;

    fn composite_context(workspace: &Path) -> ToolContext {
        ToolContext {
            execution: Default::default(),
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
        let html = "<!doctype html><html><body><h1>Title</h1>\
                    <p>Hello <strong>world</strong></p>\
                    <ul><li>one</li><li>two</li></ul>\
                    <script>ignore me</script></body></html>";
        let markdown = htmd::convert(html).unwrap();
        assert!(markdown.contains("# Title"), "{markdown}");
        assert!(markdown.contains("**world**"), "{markdown}");
        assert!(markdown.contains("one"), "{markdown}");
        assert!(!markdown.contains("script"), "{markdown}");
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
            workspace: workspace.clone(),
            client: web_client(),
            skills: None,
            skill_catalog: Arc::from(""),
            instructions: Arc::new(InstructionDiscovery::default()),
        };
        let output = tool
            .execute(
                json!({"url": format!("http://{address}/start")}),
                ToolContext {
                    execution: Default::default(),
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
        let file = std::fs::File::create(workspace.join("large.png")).unwrap();
        file.set_len(11 * 1024 * 1024).unwrap();
        let registry = NativeToolRegistry::new(workspace.clone());
        let mut context = composite_context(&workspace);
        assert!(
            registry
                .get("read")
                .unwrap()
                .execute(json!({"path":"large.png"}), context.clone())
                .await
                .is_ok()
        );
        context.execution.media_max_bytes = 10 * 1024 * 1024;
        assert!(
            registry
                .get("read")
                .unwrap()
                .execute(json!({"path":"large.png"}), context.clone())
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
