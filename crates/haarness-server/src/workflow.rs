//! Workflow-to-agent-runtime bridge.
//!
//! The isolated workflow child never receives a raw tool registry or provider
//! handle. Every host call returns through `AgentRuntime`, preserving the same
//! transcript, hooks, permission, snapshot, and cancellation paths as an
//! interactive turn.

use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    sync::{Arc, RwLock, Weak},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use haarness_core::{AgentRuntime, CoreError};
use haarness_protocol::{
    CreateThreadRequest, ItemData, QueueInputRequest, RequestId, StartTurnRequest, ThreadId,
    TurnId, TurnStatus, UpdateThreadRequest, WorkflowRun, WorkflowRunStatus,
};
use haarness_workflows::{
    HostCall, HostError, HostRequestMethod, WorkflowAgentDefaults, WorkflowModelResolution,
    WorkflowRuntimeHost,
};
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

const WORKFLOW_NOTIFICATION_BODY_LIMIT: usize = 8_000;
const CANCELLED_WORKFLOW_NOTICE: &str =
    "This workflow run was stopped intentionally. Do not restart or retry it.";

/// Attachable workflow host used to break the composition cycle between the
/// runtime, its tool registry, and the workflow coordinator.
///
/// Construct this first, pass it to `WorkflowCoordinator`, build the runtime,
/// then call [`Self::attach`]. Only a weak reference is retained, so the host
/// cannot keep a stopped application alive.
pub struct WorkflowRuntimeAdapter {
    runtime: RwLock<Weak<AgentRuntime>>,
    continuations: Mutex<ContinuationState>,
    /// Parallel workflow agents share one durable run thread for host-side
    /// file-context reads. Serialize only the short read/list materialization
    /// window so transcript items and permission prompts stay ordered; child
    /// provider turns remain fully parallel after this guard is released.
    file_context_gates: Mutex<HashMap<ThreadId, Weak<Mutex<()>>>>,
}

impl WorkflowRuntimeAdapter {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            runtime: RwLock::new(Weak::new()),
            continuations: Mutex::new(ContinuationState::default()),
            file_context_gates: Mutex::new(HashMap::new()),
        })
    }

    /// Attach the fully composed runtime. Reattaching replaces only the weak
    /// pointer; it does not extend the runtime's lifetime.
    pub fn attach(&self, runtime: &Arc<AgentRuntime>) {
        *self
            .runtime
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::downgrade(runtime);
    }

    fn runtime(&self) -> Result<Arc<AgentRuntime>, HostError> {
        self.runtime
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .upgrade()
            .ok_or_else(|| HostError::new("workflow runtime is not attached or has stopped"))
    }

    async fn direct_tool(
        &self,
        thread_id: ThreadId,
        name: &str,
        arguments: Value,
        timeout_ms: Option<u64>,
        cancellation: &CancellationToken,
    ) -> Result<haarness_core::DirectToolResult, HostError> {
        let runtime = self.runtime()?;
        let tool_cancel = cancellation.child_token();
        let call = runtime.run_direct_tool(thread_id, name, arguments, tool_cancel.clone());
        tokio::pin!(call);
        match timeout_ms {
            Some(timeout_ms) => {
                let timeout_ms = timeout_ms.clamp(1, 24 * 60 * 60 * 1_000);
                tokio::select! {
                    () = cancellation.cancelled() => {
                        tool_cancel.cancel();
                        Err(HostError::new("workflow host request was cancelled"))
                    }
                    () = tokio::time::sleep(Duration::from_millis(timeout_ms)) => {
                        tool_cancel.cancel();
                        Err(HostError::new(format!("workflow tool {name} timed out after {timeout_ms}ms")))
                    }
                    result = &mut call => result.map_err(host_error),
                }
            }
            None => tokio::select! {
                () = cancellation.cancelled() => {
                    tool_cancel.cancel();
                    Err(HostError::new("workflow host request was cancelled"))
                }
                result = &mut call => result.map_err(host_error),
            },
        }
    }

    async fn request_tool(
        &self,
        thread_id: ThreadId,
        params: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Option<Value>, HostError> {
        let params = object(params, "tool parameters")?;
        let name = required_string(params, "name", "tool name")?;
        let arguments = params.get("args").cloned().unwrap_or_else(|| json!({}));
        if !arguments.is_object() {
            return Err(HostError::new("workflow tool arguments must be an object"));
        }
        let options = optional_object(params.get("options"), "tool options")?;
        let timeout_ms = optional_u64(options, "timeout", "tool timeout")?;
        let null_on_error = on_error_null(options)?;
        match self
            .direct_tool(thread_id, name, arguments, timeout_ms, cancellation)
            .await
        {
            Ok(result) if !result.is_error => Ok(Some(json!({
                "output": result.output.text,
                "metadata": result.output.metadata,
            }))),
            Ok(result) if null_on_error && !cancellation.is_cancelled() => {
                let _ = result;
                Ok(Some(Value::Null))
            }
            Ok(result) => Err(HostError::new(result.output.text)),
            Err(_error) if null_on_error && !cancellation.is_cancelled() => Ok(Some(Value::Null)),
            Err(error) => Err(error),
        }
    }

    async fn request_shell(
        &self,
        thread_id: ThreadId,
        params: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Option<Value>, HostError> {
        let params = object(params, "shell parameters")?;
        let command = required_string(params, "command", "shell command")?;
        if command.is_empty() {
            return Err(HostError::new("workflow shell command cannot be empty"));
        }
        let options = optional_object(params.get("opts"), "shell options")?;
        let timeout_ms = optional_u64(options, "timeout", "shell timeout")?;
        let mut arguments = json!({"command": command});
        if let Some(timeout_ms) = timeout_ms {
            arguments["timeout_ms"] = timeout_ms.into();
        }
        if let Some(cwd) = options.get("cwd") {
            let cwd = cwd
                .as_str()
                .ok_or_else(|| HostError::new("workflow shell cwd must be a string"))?;
            arguments["workdir"] = cwd.into();
        }
        let result = self
            .direct_tool(thread_id, "bash", arguments, timeout_ms, cancellation)
            .await?;
        if result.is_error {
            return Err(HostError::new(result.output.text));
        }
        let (output, parsed_exit) = parse_shell_output(&result.output.text);
        let exit_code = result
            .output
            .metadata
            .get("exit")
            .and_then(Value::as_i64)
            .unwrap_or(parsed_exit);
        Ok(Some(json!({"output": output, "exitCode": exit_code})))
    }

    async fn request_agent(
        &self,
        parent_id: ThreadId,
        params: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Option<Value>, HostError> {
        let input = AgentInput::parse(params, None)?;
        let null_on_error = input.on_error == OnError::Null;
        let result = self
            .start_agent(parent_id, parent_id, None, &input, cancellation)
            .await;
        let (thread_id, result) = match result {
            Ok(result) => result,
            Err(_error) if null_on_error && !cancellation.is_cancelled() => {
                return Ok(Some(Value::Null));
            }
            Err(error) => return Err(error),
        };
        let token = uuid::Uuid::now_v7().to_string();
        self.continuations.lock().await.open.insert(
            token.clone(),
            Continuation {
                thread_id,
                authority_thread_id: parent_id,
                defaults: input.defaults(),
                active: false,
                closed: false,
            },
        );
        Ok(Some(agent_reply(result, token, thread_id)))
    }

    async fn materialize_agent_context(
        &self,
        authority_thread_id: ThreadId,
        skills: &[String],
        files: &[String],
        cancellation: &CancellationToken,
    ) -> Result<MaterializedAgentContext, HostError> {
        if skills.is_empty() && files.is_empty() {
            return Ok(MaterializedAgentContext::default());
        }
        let gate = {
            let mut gates = self.file_context_gates.lock().await;
            gates.retain(|_, gate| gate.strong_count() > 0);
            gates
                .get(&authority_thread_id)
                .and_then(Weak::upgrade)
                .unwrap_or_else(|| {
                    let gate = Arc::new(Mutex::new(()));
                    gates.insert(authority_thread_id, Arc::downgrade(&gate));
                    gate
                })
        };
        let _guard = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                return Err(HostError::new("workflow agent file context was cancelled"));
            }
            guard = gate.lock() => guard,
        };
        let mut skill_entries = Vec::with_capacity(skills.len());
        for name in skills {
            if cancellation.is_cancelled() {
                return Err(HostError::new("workflow agent context was cancelled"));
            }
            if name.trim().is_empty() {
                return Err(HostError::new("workflow agent skill names cannot be empty"));
            }
            let loaded = self
                .direct_tool(
                    authority_thread_id,
                    "skill",
                    json!({"name": name}),
                    None,
                    cancellation,
                )
                .await
                .map_err(|error| skill_context_error(name, error))?;
            if loaded.is_error {
                return Err(skill_context_error(
                    name,
                    HostError::new(loaded.output.text),
                ));
            }
            skill_entries.push((name.clone(), loaded.output.text));
        }
        let mut entries = Vec::with_capacity(files.len());
        let mut attachments = Vec::new();
        for source in files {
            if cancellation.is_cancelled() {
                return Err(HostError::new("workflow agent file context was cancelled"));
            }
            if source.trim().is_empty() {
                return Err(HostError::new(
                    "workflow agent file context paths cannot be empty",
                ));
            }
            let read = self
                .direct_tool(
                    authority_thread_id,
                    "read",
                    json!({"path": source, "_workflowFileContext": true}),
                    None,
                    cancellation,
                )
                .await
                .map_err(|error| file_context_error(source, "read", error))?;
            if read.is_error {
                return Err(file_context_error(
                    source,
                    "read",
                    HostError::new(read.output.text),
                ));
            }
            match read
                .output
                .metadata
                .get("workflowFileContext")
                .and_then(Value::as_str)
            {
                Some("directory") => {
                    let listed = self
                        .direct_tool(
                            authority_thread_id,
                            "list",
                            json!({"path": source}),
                            None,
                            cancellation,
                        )
                        .await
                        .map_err(|error| file_context_error(source, "list", error))?;
                    if listed.is_error {
                        return Err(file_context_error(
                            source,
                            "list",
                            HostError::new(listed.output.text),
                        ));
                    }
                    entries.push(FileContextEntry::Directory {
                        path: source.clone(),
                        content: join_context_output(&read.output.text, &listed.output.text),
                    });
                }
                Some("media") => {
                    let path = read
                        .output
                        .metadata
                        .get("path")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            HostError::new(format!(
                                "workflow agent file context `{source}` did not return a canonical media path"
                            ))
                        })?;
                    let mime = read
                        .output
                        .metadata
                        .get("mime")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            HostError::new(format!(
                                "workflow agent file context `{source}` did not return a media type"
                            ))
                        })?;
                    attachments.push(path.to_owned());
                    entries.push(FileContextEntry::Media {
                        path: source.clone(),
                        mime: mime.to_owned(),
                        context: read.output.text,
                    });
                }
                Some(kind) => {
                    return Err(HostError::new(format!(
                        "workflow agent file context `{source}` returned an unknown kind `{kind}`"
                    )));
                }
                None => entries.push(FileContextEntry::Text {
                    path: source.clone(),
                    content: read.output.text,
                }),
            }
        }
        Ok(MaterializedAgentContext {
            skill_prompt: render_skill_context(&skill_entries),
            file_prompt: render_file_context(&entries),
            attachments,
        })
    }

    async fn request_followup(
        &self,
        params: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Option<Value>, HostError> {
        let params = object(params, "agent follow-up parameters")?;
        let token = required_string(params, "token", "agent continuation token")?.to_owned();
        let raw = params
            .get("input")
            .ok_or_else(|| HostError::new("agent follow-up input is required"))?;
        let (authority_thread_id, thread_id, input) = {
            let mut state = self.continuations.lock().await;
            if state.closed.contains(&token) {
                return Err(HostError::new("workflow agent continuation is closed"));
            }
            let continuation = state
                .open
                .get_mut(&token)
                .ok_or_else(|| HostError::new("unknown or expired workflow agent continuation"))?;
            if continuation.closed {
                return Err(HostError::new("workflow agent continuation is closed"));
            }
            if continuation.active {
                return Err(HostError::new(
                    "workflow agent continuation already has an active follow-up",
                ));
            }
            let input = AgentInput::parse_followup(raw, &continuation.defaults)?;
            continuation.active = true;
            (
                continuation.authority_thread_id,
                continuation.thread_id,
                input,
            )
        };

        let null_on_error = input.on_error == OnError::Null;
        let result = self
            .start_agent(
                authority_thread_id,
                thread_id,
                Some(thread_id),
                &input,
                cancellation,
            )
            .await;
        {
            let mut state = self.continuations.lock().await;
            if let Some(continuation) = state.open.get_mut(&token) {
                continuation.active = false;
            }
        }
        match result {
            Ok((thread_id, result)) => Ok(Some(agent_reply(result, token, thread_id))),
            Err(_error) if null_on_error && !cancellation.is_cancelled() => Ok(Some(Value::Null)),
            Err(error) => Err(error),
        }
    }

    async fn request_close(
        &self,
        params: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Option<Value>, HostError> {
        let params = object(params, "agent close parameters")?;
        let token = required_string(params, "token", "agent continuation token")?.to_owned();
        let thread_id = loop {
            let mut state = self.continuations.lock().await;
            if state.closed.contains(&token) {
                return Ok(None);
            }
            let continuation = state
                .open
                .get_mut(&token)
                .ok_or_else(|| HostError::new("unknown or expired workflow agent continuation"))?;
            continuation.closed = true;
            if !continuation.active {
                break continuation.thread_id;
            }
            drop(state);
            tokio::select! {
                () = cancellation.cancelled() => {
                    return Err(HostError::new("workflow host request was cancelled"));
                }
                () = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
        };

        self.runtime()?
            .update_thread(
                thread_id,
                UpdateThreadRequest {
                    title: None,
                    archived: Some(true),
                    agent: None,
                    provider: None,
                    model: None,
                    ultracode: None,
                },
            )
            .await
            .map_err(host_error)?;
        let mut state = self.continuations.lock().await;
        state.open.remove(&token);
        if state.closed.len() >= 10_000 {
            state.closed.clear();
        }
        state.closed.insert(token);
        Ok(None)
    }

    async fn start_agent(
        &self,
        authority_thread_id: ThreadId,
        parent_id: ThreadId,
        existing_thread: Option<ThreadId>,
        input: &AgentInput,
        cancellation: &CancellationToken,
    ) -> Result<(ThreadId, AgentResult), HostError> {
        if cancellation.is_cancelled() {
            return Err(HostError::new("workflow agent request was cancelled"));
        }
        let runtime = self.runtime()?;
        let parent = runtime.get_thread(parent_id).await.map_err(host_error)?;
        let agent = input.agent.clone().unwrap_or_else(|| parent.agent.clone());
        if !runtime
            .agent_profiles()
            .iter()
            .any(|profile| profile.id == agent)
        {
            return Err(HostError::new(format!("unknown workflow agent: {agent}")));
        }
        let model_resolution = resolve_available_model(
            &runtime,
            &parent.provider,
            &parent.model,
            input.model.as_deref(),
        )?;
        let (provider, model) = resolve_model(
            Some(&model_resolution.resolved),
            &parent.provider,
            &parent.model,
        )?;
        // Resolve and materialize before creating/starting the child turn.
        // Every workspace access is a normal direct `read`/`list` on the
        // workflow run thread, so hooks, permission ceilings, cancellation,
        // transcript persistence, and native truncation all remain intact.
        let context = self
            .materialize_agent_context(
                authority_thread_id,
                &input.skills,
                &input.files,
                cancellation,
            )
            .await?;
        let ultracode = input.variant.as_deref() == Some("ultracode") || parent.ultracode;
        let thread_id = if let Some(thread_id) = existing_thread {
            runtime
                .update_thread(
                    thread_id,
                    UpdateThreadRequest {
                        title: None,
                        archived: Some(false),
                        agent: Some(agent.clone()),
                        provider: Some(provider.clone()),
                        model: Some(model.clone()),
                        ultracode: Some(ultracode),
                    },
                )
                .await
                .map_err(host_error)?;
            thread_id
        } else {
            runtime
                .create_thread(CreateThreadRequest {
                    schedule: None,
                    title: Some(
                        input
                            .label
                            .clone()
                            .unwrap_or_else(|| format!("Workflow agent · {agent}")),
                    ),
                    parent_id: Some(parent_id),
                    agent: Some(agent.clone()),
                    provider: Some(provider.clone()),
                    model: Some(model.clone()),
                    ultracode: Some(ultracode),
                })
                .await
                .map_err(host_error)?
                .id
        };

        let text = agent_prompt(
            &input.prompt,
            &context.skill_prompt,
            &context.file_prompt,
            input.schema.as_ref(),
        );
        let turn = runtime
            .start_workflow_agent_turn(
                authority_thread_id,
                thread_id,
                StartTurnRequest {
                    request_id: RequestId::new(),
                    text,
                    attachments: context.attachments,
                    synthetic: false,
                    auto_approve: false,
                    agent: Some(agent),
                    provider: Some(provider),
                    model: Some(model),
                    reasoning_effort: input.variant.clone(),
                    // A workflow agent is already inside an orchestration
                    // boundary. Giving it the workflow tool implicitly lets
                    // a broad audit recursively fan out into more workflows,
                    // which multiplies agents, polling, context, and cost.
                    // Nested orchestration remains available, but only when
                    // the workflow author opts in with
                    // `tools: { workflow: true }` on that agent call.
                    tools: Some(workflow_agent_tools(input.tools.as_ref())),
                    command: None,
                },
                input.schema.clone(),
            )
            .await
            .map_err(host_error)?;
        let result = wait_for_agent(&runtime, turn.id, input.schema.as_ref(), cancellation).await?;
        Ok((thread_id, result))
    }
}

fn workflow_agent_tools(configured: Option<&HashMap<String, bool>>) -> HashMap<String, bool> {
    let mut tools = configured.cloned().unwrap_or_default();
    tools.entry("workflow".into()).or_insert(false);
    tools
}

#[async_trait]
impl WorkflowRuntimeHost for WorkflowRuntimeAdapter {
    async fn create_run_thread(
        &self,
        parent: Option<ThreadId>,
        title: &str,
    ) -> Result<ThreadId, HostError> {
        let runtime = self.runtime()?;
        let inherited = match parent {
            Some(parent) => Some(runtime.get_thread(parent).await.map_err(host_error)?),
            None => None,
        };
        runtime
            .create_thread(CreateThreadRequest {
                schedule: None,
                title: Some(title.to_owned()),
                parent_id: parent,
                agent: inherited.as_ref().map(|thread| thread.agent.clone()),
                provider: inherited.as_ref().map(|thread| thread.provider.clone()),
                model: inherited.as_ref().map(|thread| thread.model.clone()),
                ultracode: inherited.as_ref().map(|thread| thread.ultracode),
            })
            .await
            .map(|thread| thread.id)
            .map_err(host_error)
    }

    async fn request(
        &self,
        thread_id: ThreadId,
        call: HostCall,
    ) -> Result<Option<Value>, HostError> {
        match call.method {
            HostRequestMethod::Tool => {
                self.request_tool(thread_id, &call.params, &call.cancellation)
                    .await
            }
            HostRequestMethod::Shell => {
                self.request_shell(thread_id, &call.params, &call.cancellation)
                    .await
            }
            HostRequestMethod::Agent => {
                self.request_agent(thread_id, &call.params, &call.cancellation)
                    .await
            }
            HostRequestMethod::AgentFollowup => {
                self.request_followup(&call.params, &call.cancellation)
                    .await
            }
            HostRequestMethod::AgentClose => {
                self.request_close(&call.params, &call.cancellation).await
            }
            HostRequestMethod::Workflow => Err(HostError::new(
                "nested workflows must be handled by WorkflowCoordinator",
            )),
        }
    }

    async fn resolve_agent_model(
        &self,
        launch_thread_id: ThreadId,
        requested: &str,
    ) -> Result<WorkflowModelResolution, HostError> {
        let runtime = self.runtime()?;
        let launch = runtime
            .get_thread(launch_thread_id)
            .await
            .map_err(host_error)?;
        resolve_available_model(&runtime, &launch.provider, &launch.model, Some(requested))
    }

    async fn workflow_agent_defaults(
        &self,
        launch_thread_id: ThreadId,
    ) -> Result<WorkflowAgentDefaults, HostError> {
        let runtime = self.runtime()?;
        let launch = runtime
            .get_thread(launch_thread_id)
            .await
            .map_err(host_error)?;
        Ok(WorkflowAgentDefaults {
            agent: Some(launch.agent),
            model: Some(format!("{}/{}", launch.provider, launch.model)),
            variant: launch.ultracode.then(|| "ultracode".to_owned()),
        })
    }

    async fn notify_terminal(
        &self,
        launch_thread_id: ThreadId,
        run: WorkflowRun,
    ) -> Result<(), HostError> {
        let Some(text) = render_terminal_notification(&run) else {
            return Ok(());
        };
        let runtime = self.runtime()?;
        let request = QueueInputRequest {
            delivery: haarness_protocol::InputDelivery::Steer,
            request_id: RequestId::new(),
            text,
            attachments: Vec::new(),
            synthetic: true,
        };
        // A launcher wait can be aborted by the same turn that is currently
        // executing this tool. Deliver out of band so that turn can unwind;
        // queue_input asks us to retry during the narrow stopping window, then
        // starts a clean follow-up turn once the interrupted turn is gone.
        tokio::spawn(async move {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match runtime.queue_input(launch_thread_id, request.clone()).await {
                    Ok(_) => return,
                    Err(CoreError::Conflict(_)) if Instant::now() < deadline => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(error) => {
                        tracing::warn!(
                            thread = %launch_thread_id,
                            %error,
                            "failed to inject workflow terminal result"
                        );
                        return;
                    }
                }
            }
        });
        Ok(())
    }

    async fn dispose_run_thread(&self, thread_id: ThreadId) -> Result<(), HostError> {
        let child_threads = {
            let mut state = self.continuations.lock().await;
            let tokens = state
                .open
                .iter()
                .filter_map(|(token, continuation)| {
                    (continuation.authority_thread_id == thread_id).then_some(token.clone())
                })
                .collect::<Vec<_>>();
            let mut child_threads = HashSet::new();
            if state.closed.len() >= 10_000 {
                state.closed.clear();
            }
            for token in tokens {
                if let Some(continuation) = state.open.remove(&token) {
                    child_threads.insert(continuation.thread_id);
                }
                state.closed.insert(token);
            }
            child_threads
        };

        let runtime = self.runtime()?;
        let mut first_error = None;
        for child_thread in child_threads {
            if let Err(error) = runtime
                .update_thread(
                    child_thread,
                    UpdateThreadRequest {
                        title: None,
                        archived: Some(true),
                        agent: None,
                        provider: None,
                        model: None,
                        ultracode: None,
                    },
                )
                .await
                && first_error.is_none()
            {
                first_error = Some(host_error(error));
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

fn render_terminal_notification(run: &WorkflowRun) -> Option<String> {
    let (state, summary, tag, raw) = match run.status {
        WorkflowRunStatus::Completed => (
            "completed",
            "Background workflow completed",
            "workflow_result",
            run.result.as_ref().map_or_else(String::new, render_value),
        ),
        WorkflowRunStatus::Failed => (
            "failed",
            "Background workflow failed",
            "workflow_error",
            run.error.clone().unwrap_or_default(),
        ),
        WorkflowRunStatus::Cancelled => (
            "cancelled",
            "Background workflow cancelled",
            "workflow_result",
            CANCELLED_WORKFLOW_NOTICE.to_owned(),
        ),
        WorkflowRunStatus::Running | WorkflowRunStatus::Paused | WorkflowRunStatus::Interrupted => {
            return None;
        }
    };
    let body = cap_terminal_body(run.id, raw);
    let name = escape_xml_attribute(&run.workflow_name);
    let summary_name = escape_xml_text(&run.workflow_name);
    let mut lines = vec![
        format!(
            "<workflow id=\"{}\" name=\"{name}\" state=\"{state}\">",
            run.id
        ),
        format!(
            "<summary>{summary}: {summary_name} (run {})</summary>",
            run.id
        ),
    ];
    if let Some(phase) = run.phase.as_deref() {
        lines.push(format!("<phase>{}</phase>", escape_xml_text(phase)));
    }
    lines.extend([
        format!("<{tag}>"),
        body,
        format!("</{tag}>"),
        "</workflow>".to_owned(),
    ]);
    Some(lines.join("\n"))
}

fn render_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        value => serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
    }
}

fn cap_terminal_body(run_id: haarness_protocol::WorkflowId, body: String) -> String {
    if body.chars().count() <= WORKFLOW_NOTIFICATION_BODY_LIMIT {
        return body;
    }
    format!(
        "{}\n[truncated — call the workflow tool with action=inspect runID={run_id} for the full result]",
        body.chars()
            .take(WORKFLOW_NOTIFICATION_BODY_LIMIT)
            .collect::<String>()
    )
}

fn escape_xml_attribute(value: &str) -> String {
    escape_xml_text(value)
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn escape_xml_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[derive(Default)]
struct ContinuationState {
    open: HashMap<String, Continuation>,
    closed: HashSet<String>,
}

struct Continuation {
    thread_id: ThreadId,
    authority_thread_id: ThreadId,
    defaults: AgentDefaults,
    active: bool,
    closed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum OnError {
    #[default]
    Fail,
    Null,
}

#[derive(Clone)]
struct AgentDefaults {
    tools: Option<HashMap<String, bool>>,
    schema: Option<Value>,
    on_error: OnError,
    agent: Option<String>,
    model: Option<String>,
    variant: Option<String>,
}

struct AgentInput {
    prompt: String,
    agent: Option<String>,
    model: Option<String>,
    variant: Option<String>,
    tools: Option<HashMap<String, bool>>,
    skills: Vec<String>,
    files: Vec<String>,
    schema: Option<Value>,
    on_error: OnError,
    label: Option<String>,
}

#[derive(Default)]
struct MaterializedAgentContext {
    skill_prompt: String,
    file_prompt: String,
    attachments: Vec<String>,
}

enum FileContextEntry {
    Text {
        path: String,
        content: String,
    },
    Directory {
        path: String,
        content: String,
    },
    Media {
        path: String,
        mime: String,
        context: String,
    },
}

impl AgentInput {
    fn parse(value: &Value, defaults: Option<&AgentDefaults>) -> Result<Self, HostError> {
        if let Some(prompt) = value.as_str() {
            return Ok(Self {
                prompt: prompt.to_owned(),
                agent: defaults.and_then(|value| value.agent.clone()),
                model: defaults.and_then(|value| value.model.clone()),
                variant: defaults.and_then(|value| value.variant.clone()),
                tools: defaults.and_then(|value| value.tools.clone()),
                skills: Vec::new(),
                files: Vec::new(),
                schema: defaults.and_then(|value| value.schema.clone()),
                on_error: defaults.map_or(OnError::Fail, |value| value.on_error),
                label: None,
            });
        }
        let value = object(value, "agent input")?;
        let prompt = required_string(value, "prompt", "agent prompt")?.to_owned();
        let tools = match value.get("tools") {
            Some(tools) => {
                let tools = object(tools, "workflow agent tools")?;
                if tools.values().any(|enabled| !enabled.is_boolean()) {
                    return Err(HostError::new(
                        "workflow agent tools must contain boolean values",
                    ));
                }
                Some(
                    tools
                        .iter()
                        .map(|(name, enabled)| {
                            (name.clone(), enabled.as_bool().expect("validated boolean"))
                        })
                        .collect(),
                )
            }
            None => defaults.and_then(|value| value.tools.clone()),
        };
        let schema = match value.get("schema") {
            Some(Value::Null) => None,
            Some(schema) if schema.is_object() => {
                validate_agent_schema_definition(schema)?;
                Some(schema.clone())
            }
            Some(_) => {
                return Err(HostError::new(
                    "workflow agent schema must be a JSON Schema object or null",
                ));
            }
            None => defaults.and_then(|value| value.schema.clone()),
        };
        Ok(Self {
            prompt,
            agent: optional_string(value, "agent")?
                .map(str::to_owned)
                .or_else(|| defaults.and_then(|value| value.agent.clone())),
            model: optional_string(value, "model")?
                .map(str::to_owned)
                .or_else(|| defaults.and_then(|value| value.model.clone())),
            variant: optional_string(value, "variant")?
                .map(str::to_owned)
                .or_else(|| defaults.and_then(|value| value.variant.clone())),
            tools,
            skills: string_array(value, "skills")?,
            files: string_array(value, "files")?,
            schema,
            on_error: parse_on_error(value.get("onError"))?
                .or_else(|| defaults.map(|value| value.on_error))
                .unwrap_or_default(),
            label: optional_string(value, "label")?.map(str::to_owned),
        })
    }

    fn parse_followup(value: &Value, defaults: &AgentDefaults) -> Result<Self, HostError> {
        if let Some(input) = value.as_object() {
            const ALLOWED: [&str; 6] = ["prompt", "tools", "skills", "files", "schema", "onError"];
            let unsupported = input
                .keys()
                .filter(|key| !ALLOWED.contains(&key.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            if !unsupported.is_empty() {
                return Err(HostError::new(format!(
                    "unsupported workflow agent follow-up options: {}",
                    unsupported.join(", ")
                )));
            }
        }
        Self::parse(value, Some(defaults))
    }

    fn defaults(&self) -> AgentDefaults {
        AgentDefaults {
            tools: self.tools.clone(),
            schema: self.schema.clone(),
            on_error: self.on_error,
            agent: self.agent.clone(),
            model: self.model.clone(),
            variant: self.variant.clone(),
        }
    }
}

struct AgentResult {
    text: String,
    data: Value,
}

fn agent_reply(result: AgentResult, continuation: String, thread_id: ThreadId) -> Value {
    json!({
        "text": result.text,
        "data": result.data,
        "continuation": continuation,
        "threadID": thread_id,
    })
}

async fn wait_for_agent(
    runtime: &Arc<AgentRuntime>,
    turn_id: TurnId,
    schema: Option<&Value>,
    cancellation: &CancellationToken,
) -> Result<AgentResult, HostError> {
    loop {
        let turn = runtime.turn(turn_id).await.map_err(host_error)?;
        match turn.status {
            TurnStatus::Completed => {
                let text = runtime
                    .items(turn.thread_id, None)
                    .await
                    .map_err(host_error)?
                    .into_iter()
                    .rev()
                    .find_map(|item| {
                        (item.turn_id == Some(turn_id))
                            .then_some(item.data)
                            .and_then(|data| {
                                if let ItemData::AssistantMessage { text } = data {
                                    Some(text)
                                } else {
                                    None
                                }
                            })
                    })
                    .unwrap_or_default();
                let data = match schema {
                    Some(schema) => {
                        let data = serde_json::from_str(text.trim()).map_err(|error| {
                            HostError::new(format!(
                                "workflow agent did not return valid structured JSON: {error}"
                            ))
                        })?;
                        validate_agent_schema(schema, &data)?;
                        data
                    }
                    None => Value::String(text.clone()),
                };
                return Ok(AgentResult { text, data });
            }
            TurnStatus::Failed => {
                return Err(HostError::new(
                    turn.error
                        .unwrap_or_else(|| "workflow agent turn failed".into()),
                ));
            }
            TurnStatus::Interrupted => {
                return Err(HostError::new(
                    turn.error
                        .unwrap_or_else(|| "workflow agent turn was interrupted".into()),
                ));
            }
            TurnStatus::Queued | TurnStatus::Running => {}
        }
        tokio::select! {
            () = cancellation.cancelled() => {
                let _ = runtime.interrupt(turn_id).await;
                return Err(HostError::new("workflow agent request was cancelled"));
            }
            () = tokio::time::sleep(Duration::from_millis(25)) => {}
        }
    }
}

fn resolve_model(
    requested: Option<&str>,
    default_provider: &str,
    default_model: &str,
) -> Result<(String, String), HostError> {
    let Some(requested) = requested else {
        return Ok((default_provider.to_owned(), default_model.to_owned()));
    };
    if requested.trim().is_empty() {
        return Err(HostError::new("workflow agent model cannot be empty"));
    }
    let resolved = requested.split_once('/').map_or_else(
        || (default_provider.to_owned(), requested.to_owned()),
        |(provider, model)| (provider.to_owned(), model.to_owned()),
    );
    if resolved.0.trim().is_empty() || resolved.1.trim().is_empty() {
        return Err(HostError::new(
            "workflow agent model must be a model id or provider/model",
        ));
    }
    Ok(resolved)
}

fn resolve_available_model(
    runtime: &AgentRuntime,
    default_provider: &str,
    default_model: &str,
    requested: Option<&str>,
) -> Result<WorkflowModelResolution, HostError> {
    let (provider, model) = resolve_model(requested, default_provider, default_model)?;
    let resolved = format!("{provider}/{model}");
    let requested_is_launch_model = provider == default_provider && model == default_model;
    if requested.is_none()
        || requested_is_launch_model
        || provider_model_available(runtime, &provider, &model)
    {
        return Ok(WorkflowModelResolution {
            resolved,
            unavailable_requested: None,
        });
    }
    if runtime.providers().get(default_provider).is_none() {
        return Err(HostError::new(format!(
            "workflow launch provider {default_provider} is unavailable"
        )));
    }
    Ok(WorkflowModelResolution {
        resolved: format!("{default_provider}/{default_model}"),
        unavailable_requested: requested.map(str::to_owned),
    })
}

fn provider_model_available(runtime: &AgentRuntime, provider: &str, model: &str) -> bool {
    let Some(provider) = runtime.providers().get(provider) else {
        return false;
    };
    let models = provider.info().models;
    models.is_empty() || models.iter().any(|candidate| candidate.id == model)
}

fn validate_agent_schema_definition(schema: &Value) -> Result<(), HostError> {
    jsonschema::validator_for(schema)
        .map(|_| ())
        .map_err(|error| HostError::new(format!("invalid workflow agent JSON Schema: {error}")))
}

fn validate_agent_schema(schema: &Value, data: &Value) -> Result<(), HostError> {
    let validator = jsonschema::validator_for(schema)
        .map_err(|error| HostError::new(format!("invalid workflow agent JSON Schema: {error}")))?;
    let errors = validator
        .iter_errors(data)
        .take(3)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(HostError::new(format!(
            "workflow agent structured JSON does not match its schema: {}",
            errors.join("; ")
        )))
    }
}

fn file_context_error(source: &str, operation: &str, error: HostError) -> HostError {
    HostError::new(format!(
        "workflow agent file context `{source}` {operation} failed: {error}"
    ))
}

fn skill_context_error(name: &str, error: HostError) -> HostError {
    HostError::new(format!(
        "workflow agent skill context `{name}` failed: {error}"
    ))
}

fn join_context_output(first: &str, second: &str) -> String {
    match (first.is_empty(), second.is_empty()) {
        (true, _) => second.to_owned(),
        (_, true) => first.to_owned(),
        (false, false) => format!("{first}\n{second}"),
    }
}

fn render_skill_context(entries: &[(String, String)]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let mut result = String::from(
        "<workflow-skill-context>\nFollow these ordered skill instructions for this agent turn.\n",
    );
    for (index, (name, content)) in entries.iter().enumerate() {
        let number = index + 1;
        let label = serde_json::to_string(name).expect("skill name serializes");
        let _ = writeln!(result, "[skill {number} name={label}]");
        let _ = writeln!(result, "{content}");
        let _ = writeln!(result, "[/skill {number}]");
    }
    result.push_str("</workflow-skill-context>");
    result
}

fn render_file_context(entries: &[FileContextEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let mut result = String::from(
        "<workflow-file-context>\nThe workflow supplied these ordered workspace entries.\n",
    );
    for (index, entry) in entries.iter().enumerate() {
        let number = index + 1;
        match entry {
            FileContextEntry::Text { path, content } => {
                let label = serde_json::to_string(path).expect("file path serializes");
                let _ = writeln!(result, "[entry {number} kind=text path={label}]");
                let _ = writeln!(result, "{content}");
                let _ = writeln!(result, "[/entry {number}]");
            }
            FileContextEntry::Directory { path, content } => {
                let label = serde_json::to_string(path).expect("directory path serializes");
                let _ = writeln!(result, "[entry {number} kind=directory path={label}]");
                let _ = writeln!(result, "{content}");
                let _ = writeln!(result, "[/entry {number}]");
            }
            FileContextEntry::Media {
                path,
                mime,
                context,
            } => {
                let label = serde_json::to_string(path).expect("media path serializes");
                let mime = serde_json::to_string(mime).expect("media type serializes");
                let _ = writeln!(
                    result,
                    "[entry {number} kind=media path={label} mime={mime} attached=true]"
                );
                if !context.is_empty() {
                    let _ = writeln!(result, "{context}");
                }
                let _ = writeln!(result, "[/entry {number}]");
            }
        }
    }
    result.push_str("</workflow-file-context>");
    result
}

fn agent_prompt(
    prompt: &str,
    skill_context: &str,
    file_context: &str,
    schema: Option<&Value>,
) -> String {
    let mut result = if skill_context.is_empty() {
        prompt.to_owned()
    } else {
        format!("{skill_context}\n\n{prompt}")
    };
    if !file_context.is_empty() {
        result.push_str("\n\n");
        result.push_str(file_context);
    }
    if let Some(schema) = schema {
        let _ = write!(
            result,
            "\n\nSubmit the final answer through the StructuredOutput tool with arguments \
             matching this JSON Schema (do not finish with plain text):\n{schema}"
        );
    }
    result
}

fn parse_shell_output(text: &str) -> (String, i64) {
    let Some((first, rest)) = text.split_once('\n') else {
        return (text.to_owned(), 0);
    };
    first
        .strip_prefix("exit: ")
        .and_then(|value| value.trim().parse::<i64>().ok())
        .map_or_else(|| (text.to_owned(), 0), |exit| (rest.to_owned(), exit))
}

fn object<'a>(value: &'a Value, label: &str) -> Result<&'a Map<String, Value>, HostError> {
    value
        .as_object()
        .ok_or_else(|| HostError::new(format!("{label} must be an object")))
}

fn optional_object<'a>(
    value: Option<&'a Value>,
    label: &str,
) -> Result<&'a Map<String, Value>, HostError> {
    match value {
        None => Ok(empty_object()),
        Some(value) => object(value, label),
    }
}

fn empty_object() -> &'static Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    EMPTY.get_or_init(Map::new)
}

fn required_string<'a>(
    value: &'a Map<String, Value>,
    key: &str,
    label: &str,
) -> Result<&'a str, HostError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| HostError::new(format!("{label} is required")))
}

fn optional_string<'a>(
    value: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a str>, HostError> {
    value.get(key).map_or(Ok(None), |value| {
        value
            .as_str()
            .map(Some)
            .ok_or_else(|| HostError::new(format!("workflow agent {key} must be a string")))
    })
}

fn string_array(value: &Map<String, Value>, key: &str) -> Result<Vec<String>, HostError> {
    let Some(values) = value.get(key) else {
        return Ok(Vec::new());
    };
    let values = values
        .as_array()
        .ok_or_else(|| HostError::new(format!("workflow agent {key} must be an array")))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| HostError::new(format!("workflow agent {key} must contain strings")))
        })
        .collect()
}

fn optional_u64(
    value: &Map<String, Value>,
    key: &str,
    label: &str,
) -> Result<Option<u64>, HostError> {
    value.get(key).map_or(Ok(None), |value| {
        value
            .as_u64()
            .filter(|value| *value > 0)
            .map(Some)
            .ok_or_else(|| HostError::new(format!("{label} must be a positive integer")))
    })
}

fn parse_on_error(value: Option<&Value>) -> Result<Option<OnError>, HostError> {
    value.map_or(Ok(None), |value| match value.as_str() {
        Some("fail") => Ok(Some(OnError::Fail)),
        Some("null") => Ok(Some(OnError::Null)),
        _ => Err(HostError::new(
            "workflow onError must be either fail or null",
        )),
    })
}

fn on_error_null(options: &Map<String, Value>) -> Result<bool, HostError> {
    Ok(parse_on_error(options.get("onError"))? == Some(OnError::Null))
}

fn host_error(error: CoreError) -> HostError {
    HostError::new(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use haarness_core::{
        EventBus, ModelProvider, PermissionDecision, PermissionRule, ProviderEvent,
        ProviderMessage, ProviderRequest, ProviderStream, RuntimeConfig, SkillRegistry,
        StaticProviderRegistry,
    };
    use haarness_protocol::{ModelInfo, ProviderInfo};
    use haarness_store::SqliteStore;
    use std::sync::Mutex as StdMutex;
    use tokio::sync::Semaphore;

    #[test]
    fn shell_result_separates_the_internal_exit_prefix() {
        assert_eq!(
            parse_shell_output("exit: 17\nstdout\nstderr"),
            ("stdout\nstderr".into(), 17)
        );
        assert_eq!(
            parse_shell_output("plain output"),
            ("plain output".into(), 0)
        );
    }

    #[test]
    fn agent_inputs_preserve_continuation_defaults_and_null_override() {
        let initial = AgentInput::parse(
            &json!({
                "prompt": "first",
                "agent": "review",
                "model": "openai/gpt-5",
                "variant": "high",
                "tools": {"bash": false},
                "schema": {"type": "object"},
                "onError": "fail"
            }),
            None,
        )
        .unwrap();
        let next = AgentInput::parse(
            &json!({"prompt": "correct it", "schema": null, "onError": "null"}),
            Some(&initial.defaults()),
        )
        .unwrap();
        assert_eq!(next.agent.as_deref(), Some("review"));
        assert_eq!(next.model.as_deref(), Some("openai/gpt-5"));
        assert_eq!(next.variant.as_deref(), Some("high"));
        assert_eq!(next.tools.as_ref().unwrap().get("bash"), Some(&false));
        assert!(next.schema.is_none());
        assert_eq!(next.on_error, OnError::Null);

        let overridden = AgentInput::parse(
            &json!({"prompt": "retry", "tools": {"bash": true, "write": false}}),
            Some(&initial.defaults()),
        )
        .unwrap();
        let tools = overridden.tools.unwrap();
        assert_eq!(tools.get("bash"), Some(&true));
        assert_eq!(tools.get("write"), Some(&false));
    }

    #[test]
    fn workflow_agents_require_explicit_nested_workflow_opt_in() {
        let defaults = workflow_agent_tools(None);
        assert_eq!(defaults.get("workflow"), Some(&false));

        let configured = HashMap::from([("bash".to_owned(), false)]);
        let tools = workflow_agent_tools(Some(&configured));
        assert_eq!(tools.get("bash"), Some(&false));
        assert_eq!(tools.get("workflow"), Some(&false));

        let opted_in = HashMap::from([("workflow".to_owned(), true)]);
        let tools = workflow_agent_tools(Some(&opted_in));
        assert_eq!(tools.get("workflow"), Some(&true));
    }

    #[test]
    fn invalid_agent_options_fail_instead_of_silently_weakening_controls() {
        let error = AgentInput::parse(
            &json!({"prompt": "inspect", "tools": {"bash": "yes"}}),
            None,
        )
        .err()
        .expect("invalid input");
        assert!(error.to_string().contains("boolean"));
        let error = AgentInput::parse(&json!({"prompt": "inspect", "onError": "ignore"}), None)
            .err()
            .expect("invalid input");
        assert!(error.to_string().contains("fail or null"));

        let defaults = AgentInput::parse(
            &json!({"prompt":"first", "agent":"review", "model":"openai/gpt-5"}),
            None,
        )
        .unwrap()
        .defaults();
        for option in [
            json!({"prompt":"again", "agent":"build"}),
            json!({"prompt":"again", "model":"openai/gpt-5-mini"}),
            json!({"prompt":"again", "variant":"high"}),
            json!({"prompt":"again", "label":"replacement"}),
        ] {
            let error = AgentInput::parse_followup(&option, &defaults)
                .err()
                .expect("unsupported follow-up option");
            assert!(error.to_string().contains("unsupported"), "{error}");
        }

        for model in ["provider/", "/model"] {
            let error = resolve_model(Some(model), "provider", "default")
                .expect_err("empty model component");
            assert!(error.to_string().contains("provider/model"), "{error}");
        }
    }

    #[test]
    fn agent_json_schema_rejects_valid_json_that_violates_constraints() {
        let schema = json!({
            "type": "object",
            "properties": {"count": {"type": "integer", "minimum": 1}},
            "required": ["count"],
            "additionalProperties": false
        });
        validate_agent_schema_definition(&schema).expect("schema compiles");
        validate_agent_schema(&schema, &json!({"count": 2})).expect("matching result");
        let error = validate_agent_schema(&schema, &json!({"count": "two"}))
            .expect_err("valid JSON with the wrong type must fail schema validation");
        assert!(error.to_string().contains("does not match"), "{error}");

        let invalid = AgentInput::parse(
            &json!({"prompt":"answer", "schema":{"type":"not-a-json-schema-type"}}),
            None,
        )
        .err()
        .expect("invalid schema definition");
        assert!(
            invalid
                .to_string()
                .contains("invalid workflow agent JSON Schema")
        );
    }

    #[derive(Default)]
    struct ContextCaptureProvider {
        requests: StdMutex<Vec<ProviderRequest>>,
    }

    #[async_trait]
    impl ModelProvider for ContextCaptureProvider {
        fn id(&self) -> &str {
            "context-capture"
        }

        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                id: self.id().into(),
                name: "Workflow context fixture".into(),
                authenticated: true,
                models: vec![ModelInfo {
                    id: "fixture".into(),
                    display_name: "Fixture".into(),
                    reasoning: false,
                    reasoning_efforts: Vec::new(),
                    images: true,
                    video: true,
                    tools: true,
                    context_window: None,
                    max_output_tokens: None,
                    pricing: None,
                }],
            }
        }

        async fn stream(
            &self,
            request: ProviderRequest,
            _cancel: CancellationToken,
        ) -> Result<ProviderStream, CoreError> {
            self.requests
                .lock()
                .expect("captured requests")
                .push(request);
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderEvent::TextDelta("context received".into())),
                Ok(ProviderEvent::Finished {
                    reason: "completed".into(),
                }),
            ])))
        }
    }

    async fn context_fixture(
        workspace: &std::path::Path,
        tools: crate::NativeToolRegistry,
        provider: Arc<ContextCaptureProvider>,
        config: RuntimeConfig,
    ) -> (Arc<AgentRuntime>, Arc<WorkflowRuntimeAdapter>, ThreadId) {
        let runtime = Arc::new(AgentRuntime::with_config(
            Arc::new(SqliteStore::in_memory().unwrap()),
            Arc::new(StaticProviderRegistry::new([
                provider as Arc<dyn ModelProvider>
            ])),
            Arc::new(tools),
            EventBus::new(128),
            workspace.to_path_buf(),
            String::new(),
            config,
        ));
        let root = runtime
            .create_thread(CreateThreadRequest {
                provider: Some("context-capture".into()),
                model: Some("fixture".into()),
                ..CreateThreadRequest::default()
            })
            .await
            .unwrap();
        let host = WorkflowRuntimeAdapter::new();
        host.attach(&runtime);
        let run_thread = host
            .create_run_thread(Some(root.id), "Workflow · context fixture")
            .await
            .unwrap();
        (runtime, host, run_thread)
    }

    #[tokio::test]
    async fn workflow_agent_materializes_relative_text_directory_media_and_skill_context() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-agent-context-{}", uuid::Uuid::now_v7()));
        let skills_root = workspace.join("skills");
        tokio::fs::create_dir_all(workspace.join("src"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(skills_root.join("review"))
            .await
            .unwrap();
        tokio::fs::write(workspace.join("README.md"), "# Relative readme\n")
            .await
            .unwrap();
        tokio::fs::write(workspace.join("src/lib.rs"), "pub fn parity() {}\n")
            .await
            .unwrap();
        tokio::fs::write(workspace.join("src/z-last.txt"), "listed only\n")
            .await
            .unwrap();
        tokio::fs::write(workspace.join("pixel.png"), b"\x89PNG\r\n\x1a\n")
            .await
            .unwrap();
        tokio::fs::write(
            skills_root.join("review/SKILL.md"),
            "---\ndescription: Review deterministically\n---\nCite exact evidence.\n",
        )
        .await
        .unwrap();
        let skills = crate::FilesystemSkillRegistry::new(vec![(skills_root, true)]);
        let skill_infos = skills.discover().await.unwrap();
        let tools = crate::NativeToolRegistry::with_extras(
            workspace.clone(),
            None,
            Some(Arc::new(skills)),
            skill_infos,
        );
        let provider = Arc::new(ContextCaptureProvider::default());
        let (runtime, host, run_thread) = context_fixture(
            &workspace,
            tools,
            Arc::clone(&provider),
            RuntimeConfig::default(),
        )
        .await;

        let reply = host
            .request(
                run_thread,
                HostCall {
                    run_id: "context-run".into(),
                    request_id: 1,
                    method: HostRequestMethod::Agent,
                    params: json!({
                        "prompt": "Review the supplied context",
                        "skills": ["review"],
                        "files": ["README.md", "src/lib.rs", "src", "pixel.png"]
                    }),
                    cancellation: CancellationToken::new(),
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reply["text"], "context received");

        {
            let requests = provider.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            let (content, attachments) = requests[0]
                .messages
                .iter()
                .find_map(|message| match message {
                    ProviderMessage::Media {
                        role,
                        content,
                        attachments,
                    } if role == "user" => Some((content, attachments)),
                    _ => None,
                })
                .expect("media file keeps a multimodal user message");
            assert!(content.starts_with("<workflow-skill-context>"), "{content}");
            assert!(content.contains("[skill 1 name=\"review\"]"), "{content}");
            assert!(content.contains("Cite exact evidence."), "{content}");
            assert!(content.contains("Review the supplied context"), "{content}");
            assert!(content.contains("[entry 1 kind=text path=\"README.md\"]"));
            assert!(content.contains("# Relative readme"));
            assert!(content.contains("[entry 2 kind=text path=\"src/lib.rs\"]"));
            assert!(content.contains("pub fn parity() {}"));
            assert!(content.contains("[entry 3 kind=directory path=\"src\"]"));
            assert!(content.contains("lib.rs\nz-last.txt"));
            assert!(content.contains(
                "[entry 4 kind=media path=\"pixel.png\" mime=\"image/png\" attached=true]"
            ));
            assert_eq!(
                attachments,
                &[workspace
                    .join("pixel.png")
                    .canonicalize()
                    .unwrap()
                    .display()
                    .to_string()]
            );
        }

        let run_items = runtime.items(run_thread, None).await.unwrap();
        let direct_tools = run_items
            .iter()
            .filter_map(|item| match &item.data {
                ItemData::ToolCall { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            direct_tools,
            ["skill", "read", "read", "read", "list", "read"]
        );

        let followup = host
            .request(
                run_thread,
                HostCall {
                    run_id: "context-run".into(),
                    request_id: 2,
                    method: HostRequestMethod::AgentFollowup,
                    params: json!({
                        "token": reply["continuation"],
                        "input": {
                            "prompt": "Re-check the supplied file",
                            "skills": ["review"],
                            "files": ["README.md"]
                        }
                    }),
                    cancellation: CancellationToken::new(),
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(followup["threadID"], reply["threadID"]);
        {
            let requests = provider.requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            let followup_content = requests[1]
                .messages
                .iter()
                .rev()
                .find_map(|message| match message {
                    ProviderMessage::Text { role, content } if role == "user" => Some(content),
                    _ => None,
                })
                .expect("follow-up context is a new user turn");
            assert!(followup_content.contains("[skill 1 name=\"review\"]"));
            assert!(followup_content.contains("[entry 1 kind=text path=\"README.md\"]"));
            assert!(followup_content.contains("Re-check the supplied file"));
        }
        host.dispose_run_thread(run_thread).await.unwrap();
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    #[tokio::test]
    async fn workflow_agent_file_context_asks_for_workspace_escape_and_honors_read_denial() {
        let workspace = std::env::temp_dir().join(format!(
            "haarness-agent-context-deny-{}",
            uuid::Uuid::now_v7()
        ));
        let outside = std::env::temp_dir().join(format!(
            "haarness-agent-context-outside-{}",
            uuid::Uuid::now_v7()
        ));
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::create_dir_all(&outside).await.unwrap();
        tokio::fs::write(workspace.join("README.md"), "private\n")
            .await
            .unwrap();
        tokio::fs::write(outside.join("secret.txt"), "outside\n")
            .await
            .unwrap();

        let provider = Arc::new(ContextCaptureProvider::default());
        let (runtime, host, run_thread) = context_fixture(
            &workspace,
            crate::NativeToolRegistry::new(workspace.clone()),
            Arc::clone(&provider),
            RuntimeConfig::default(),
        )
        .await;
        let outside_request = {
            let host = Arc::clone(&host);
            let outside = outside.clone();
            tokio::spawn(async move {
                host.request(
                    run_thread,
                    HostCall {
                        run_id: "outside-run".into(),
                        request_id: 1,
                        method: HostRequestMethod::Agent,
                        params: json!({
                            "prompt":"inspect",
                            "files":[outside.join("secret.txt").display().to_string()]
                        }),
                        cancellation: CancellationToken::new(),
                    },
                )
                .await
            })
        };
        let pending = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(permission) = runtime.pending_permissions().into_iter().next() {
                    break permission;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("outside context raised a permission prompt");
        assert_eq!(pending.permission, "external_directory");
        runtime
            .reply_permission(
                &pending.permission_id,
                haarness_protocol::PermissionReply {
                    decision: haarness_protocol::PermissionReplyDecision::Reject,
                    feedback: None,
                },
            )
            .await
            .unwrap();
        let outside_error = outside_request
            .await
            .unwrap()
            .expect_err("rejected outside path must not become provider context");
        assert!(
            outside_error.to_string().contains("permission denied"),
            "{outside_error}"
        );
        assert!(provider.requests.lock().unwrap().is_empty());

        let denied_provider = Arc::new(ContextCaptureProvider::default());
        let (_denied_runtime, denied_host, denied_thread) = context_fixture(
            &workspace,
            crate::NativeToolRegistry::new(workspace.clone()),
            Arc::clone(&denied_provider),
            RuntimeConfig {
                base_rules: vec![PermissionRule {
                    pattern: "read:*".into(),
                    decision: PermissionDecision::Deny,
                }],
                ..RuntimeConfig::default()
            },
        )
        .await;
        let denied_error = denied_host
            .request(
                denied_thread,
                HostCall {
                    run_id: "denied-run".into(),
                    request_id: 1,
                    method: HostRequestMethod::Agent,
                    params: json!({"prompt":"inspect", "files":["README.md"]}),
                    cancellation: CancellationToken::new(),
                },
            )
            .await
            .expect_err("read deny ceiling must win before provider dispatch");
        assert!(
            denied_error
                .to_string()
                .contains("tool read is disabled by permission policy"),
            "{denied_error}"
        );
        assert!(denied_provider.requests.lock().unwrap().is_empty());

        tokio::fs::remove_dir_all(workspace).await.unwrap();
        tokio::fs::remove_dir_all(outside).await.unwrap();
    }

    #[tokio::test]
    async fn workflow_agent_context_cancellation_clears_permission_and_never_starts_child() {
        let workspace = std::env::temp_dir().join(format!(
            "haarness-agent-context-cancel-{}",
            uuid::Uuid::now_v7()
        ));
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::write(workspace.join("README.md"), "wait for permission\n")
            .await
            .unwrap();
        let provider = Arc::new(ContextCaptureProvider::default());
        let (runtime, host, run_thread) = context_fixture(
            &workspace,
            crate::NativeToolRegistry::new(workspace.clone()),
            Arc::clone(&provider),
            RuntimeConfig {
                base_rules: vec![PermissionRule {
                    pattern: "read:*".into(),
                    decision: PermissionDecision::Ask,
                }],
                ..RuntimeConfig::default()
            },
        )
        .await;
        let cancellation = CancellationToken::new();
        let task_host = Arc::clone(&host);
        let task_cancel = cancellation.clone();
        let task = tokio::spawn(async move {
            task_host
                .request(
                    run_thread,
                    HostCall {
                        run_id: "cancel-context-run".into(),
                        request_id: 1,
                        method: HostRequestMethod::Agent,
                        params: json!({"prompt":"inspect", "files":["README.md"]}),
                        cancellation: task_cancel,
                    },
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if !runtime.pending_permissions().is_empty() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("file context reaches the normal permission channel");
        cancellation.cancel();
        let error = task.await.unwrap().expect_err("cancelled context request");
        assert!(error.to_string().contains("cancelled"), "{error}");
        assert!(runtime.pending_permissions().is_empty());
        assert!(provider.requests.lock().unwrap().is_empty());
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    #[tokio::test]
    async fn workflow_agent_skill_context_rejects_missing_and_untrusted_skills() {
        let workspace = std::env::temp_dir().join(format!(
            "haarness-agent-skill-deny-{}",
            uuid::Uuid::now_v7()
        ));
        let skills_root = workspace.join("skills");
        tokio::fs::create_dir_all(skills_root.join("secret"))
            .await
            .unwrap();
        tokio::fs::write(
            skills_root.join("secret/SKILL.md"),
            "Do not load untrusted.\n",
        )
        .await
        .unwrap();
        let skills = crate::FilesystemSkillRegistry::new(vec![(skills_root, false)]);
        let infos = skills.discover().await.unwrap();
        let provider = Arc::new(ContextCaptureProvider::default());
        let (_runtime, host, run_thread) = context_fixture(
            &workspace,
            crate::NativeToolRegistry::with_extras(
                workspace.clone(),
                None,
                Some(Arc::new(skills)),
                infos,
            ),
            Arc::clone(&provider),
            RuntimeConfig::default(),
        )
        .await;
        for (request_id, name, expected) in [
            (1, "secret", "project configuration is disabled"),
            (2, "absent", "skill absent"),
        ] {
            let error = host
                .request(
                    run_thread,
                    HostCall {
                        run_id: "skill-deny-run".into(),
                        request_id,
                        method: HostRequestMethod::Agent,
                        params: json!({"prompt":"inspect", "skills":[name]}),
                        cancellation: CancellationToken::new(),
                    },
                )
                .await
                .expect_err("skill materialization must fail before child start");
            assert!(error.to_string().contains(expected), "{error}");
        }
        assert!(provider.requests.lock().unwrap().is_empty());
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    struct StructuredProvider {
        seen_schema: StdMutex<Option<Value>>,
        output: StdMutex<Value>,
    }

    #[async_trait]
    impl ModelProvider for StructuredProvider {
        fn id(&self) -> &str {
            "structured"
        }

        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                id: "structured".into(),
                name: "Structured workflow fixture".into(),
                authenticated: true,
                models: vec![ModelInfo {
                    id: "fixture".into(),
                    display_name: "Fixture".into(),
                    reasoning: false,
                    reasoning_efforts: Vec::new(),
                    images: false,
                    video: false,
                    tools: true,
                    context_window: None,
                    max_output_tokens: None,
                    pricing: None,
                }],
            }
        }

        async fn stream(
            &self,
            request: ProviderRequest,
            _cancel: CancellationToken,
        ) -> Result<ProviderStream, CoreError> {
            *self.seen_schema.lock().expect("schema capture") = request.output_schema;
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderEvent::StructuredOutput(
                    self.output.lock().expect("structured output").clone(),
                )),
                Ok(ProviderEvent::Finished {
                    reason: "completed".into(),
                }),
            ])))
        }
    }

    #[tokio::test]
    async fn workflow_schema_reaches_the_provider_contract_and_remains_post_validated() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-structured-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        let schema = json!({
            "type":"object",
            "properties":{"count":{"type":"integer","minimum":1}},
            "required":["count"],
            "additionalProperties":false
        });
        let provider = Arc::new(StructuredProvider {
            seen_schema: StdMutex::new(None),
            output: StdMutex::new(json!({"count": 6})),
        });
        let runtime = Arc::new(AgentRuntime::with_workspace(
            Arc::new(SqliteStore::in_memory().unwrap()),
            Arc::new(StaticProviderRegistry::new([
                provider.clone() as Arc<dyn ModelProvider>
            ])),
            Arc::new(crate::NativeToolRegistry::new(workspace.clone())),
            EventBus::new(128),
            workspace.clone(),
        ));
        let root = runtime
            .create_thread(CreateThreadRequest {
                provider: Some("structured".into()),
                model: Some("fixture".into()),
                ..CreateThreadRequest::default()
            })
            .await
            .unwrap();
        let host = WorkflowRuntimeAdapter::new();
        host.attach(&runtime);
        let reply = host
            .request(
                root.id,
                HostCall {
                    run_id: "structured-run".into(),
                    request_id: 1,
                    method: HostRequestMethod::Agent,
                    params: json!({"prompt":"count", "schema":schema}),
                    cancellation: CancellationToken::new(),
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reply["data"], json!({"count": 6}));
        assert_eq!(reply["text"], r#"{"count":6}"#);
        assert_eq!(provider.seen_schema.lock().unwrap().as_ref(), Some(&schema));

        *provider.output.lock().unwrap() = json!({"count": "six"});
        let error = host
            .request(
                root.id,
                HostCall {
                    run_id: "structured-run".into(),
                    request_id: 2,
                    method: HostRequestMethod::Agent,
                    params: json!({"prompt":"count again", "schema":schema}),
                    cancellation: CancellationToken::new(),
                },
            )
            .await
            .expect_err("provider schema enforcement is followed by host validation");
        assert!(error.to_string().contains("does not match"), "{error}");
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    struct HoldingProvider {
        started: Arc<Semaphore>,
    }

    #[async_trait]
    impl ModelProvider for HoldingProvider {
        fn id(&self) -> &str {
            "holding"
        }

        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                id: "holding".into(),
                name: "Holding workflow notification fixture".into(),
                authenticated: true,
                models: vec![ModelInfo {
                    id: "fixture".into(),
                    display_name: "Fixture".into(),
                    reasoning: false,
                    reasoning_efforts: Vec::new(),
                    images: false,
                    video: false,
                    tools: false,
                    context_window: None,
                    max_output_tokens: None,
                    pricing: None,
                }],
            }
        }

        async fn stream(
            &self,
            _request: ProviderRequest,
            cancel: CancellationToken,
        ) -> Result<ProviderStream, CoreError> {
            let started = Arc::clone(&self.started);
            Ok(Box::pin(stream::once(async move {
                started.add_permits(1);
                cancel.cancelled().await;
                Err(CoreError::Cancelled)
            })))
        }
    }

    #[tokio::test]
    async fn terminal_notification_queues_into_an_active_turn_and_caps_the_body() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-notify-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        let provider = Arc::new(HoldingProvider {
            started: Arc::new(Semaphore::new(0)),
        });
        let providers = Arc::new(StaticProviderRegistry::new([
            provider.clone() as Arc<dyn ModelProvider>
        ]));
        let repository = Arc::new(SqliteStore::in_memory().unwrap());
        let tools = Arc::new(crate::NativeToolRegistry::new(workspace.clone()));
        let runtime = Arc::new(AgentRuntime::with_workspace(
            repository,
            providers,
            tools,
            EventBus::new(128),
            workspace.clone(),
        ));
        let thread = runtime
            .create_thread(CreateThreadRequest {
                provider: Some("holding".into()),
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
                    text: "keep this turn active".into(),
                    attachments: Vec::new(),
                    synthetic: false,
                    auto_approve: false,
                    agent: None,
                    provider: None,
                    model: None,
                    reasoning_effort: None,
                    tools: None,
                    command: None,
                },
            )
            .await
            .unwrap();
        let permit = tokio::time::timeout(Duration::from_secs(2), provider.started.acquire())
            .await
            .expect("provider stream starts")
            .expect("fixture semaphore remains open");
        permit.forget();

        let now = chrono::Utc::now();
        let run: WorkflowRun = serde_json::from_value(json!({
            "id": haarness_protocol::WorkflowId::new(),
            "directory": workspace,
            "workflowName": "demo<&\"",
            "sourceHash": "",
            "status": "completed",
            "definition": {
                "name": "demo",
                "invocation": "demo",
                "dialect": "claude-portable",
                "meta": {"name": "demo"}
            },
            "result": "x".repeat(WORKFLOW_NOTIFICATION_BODY_LIMIT + 500),
            "time": {
                "created": now,
                "updated": now,
                "started": now,
                "completed": now
            }
        }))
        .unwrap();
        let run_id = run.id;
        let host = WorkflowRuntimeAdapter::new();
        host.attach(&runtime);
        host.notify_terminal(thread.id, run).await.unwrap();

        let items = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let items = runtime.items(thread.id, None).await.unwrap();
                if items.iter().any(|item| {
                    matches!(
                        item.data,
                        ItemData::UserMessage {
                            synthetic: true,
                            ..
                        }
                    )
                }) {
                    return items;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("synthetic result is queued");
        let notification = items
            .iter()
            .find_map(|item| match &item.data {
                ItemData::UserMessage {
                    text,
                    queued: true,
                    synthetic: true,
                    ..
                } if text.contains("<workflow ") => Some(text),
                _ => None,
            })
            .expect("active turn receives a queued workflow result");
        assert!(notification.contains("state=\"completed\""));
        assert!(notification.contains("name=\"demo&lt;&amp;&quot;\""));
        assert!(notification.contains(&format!("action=inspect runID={run_id}")));
        assert!(
            notification.chars().count() < WORKFLOW_NOTIFICATION_BODY_LIMIT + 500,
            "notification keeps a bounded preview"
        );

        runtime.interrupt(turn.id).await.unwrap();
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    struct FabricatingProvider {
        advertised: StdMutex<Vec<Vec<String>>>,
        tool_name: String,
        arguments: Value,
    }

    #[async_trait]
    impl ModelProvider for FabricatingProvider {
        fn id(&self) -> &str {
            "fabric"
        }

        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                id: "fabric".into(),
                name: "Workflow host fixture".into(),
                authenticated: true,
                models: vec![ModelInfo {
                    id: "fixture".into(),
                    display_name: "Fixture".into(),
                    reasoning: false,
                    reasoning_efforts: Vec::new(),
                    images: false,
                    video: false,
                    tools: true,
                    context_window: None,
                    max_output_tokens: None,
                    pricing: None,
                }],
            }
        }

        async fn stream(
            &self,
            request: ProviderRequest,
            _cancel: CancellationToken,
        ) -> Result<ProviderStream, CoreError> {
            self.advertised
                .lock()
                .expect("advertised tools")
                .push(request.tools.iter().map(|tool| tool.name.clone()).collect());
            let fabricated_call_id = format!("fabricated-{}", self.tool_name);
            let has_result = request.messages.iter().any(|message| {
                matches!(
                    message,
                    ProviderMessage::ToolResult { call_id, .. } if call_id == &fabricated_call_id
                )
            });
            let events = if has_result {
                vec![
                    Ok(ProviderEvent::TextDelta("bounded agent completed".into())),
                    Ok(ProviderEvent::Finished {
                        reason: "completed".into(),
                    }),
                ]
            } else {
                vec![
                    Ok(ProviderEvent::ToolCall {
                        call_id: fabricated_call_id,
                        name: self.tool_name.clone(),
                        arguments: self.arguments.clone(),
                        provider_metadata: Value::Null,
                    }),
                    Ok(ProviderEvent::Finished {
                        reason: "tool_calls".into(),
                    }),
                ]
            };
            Ok(Box::pin(stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn attached_host_uses_real_tool_and_agent_paths_with_enforced_tool_ceiling() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-host-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::write(workspace.join("fixture.txt"), "host tool output")
            .await
            .unwrap();
        let provider = Arc::new(FabricatingProvider {
            advertised: StdMutex::new(Vec::new()),
            tool_name: "bash".into(),
            arguments: json!({"command": "touch should-not-exist"}),
        });
        let providers = Arc::new(StaticProviderRegistry::new([
            provider.clone() as Arc<dyn ModelProvider>
        ]));
        let repository = Arc::new(SqliteStore::in_memory().unwrap());
        let tools = Arc::new(crate::NativeToolRegistry::new(workspace.clone()));
        let runtime = Arc::new(AgentRuntime::with_workspace(
            repository,
            providers,
            tools,
            EventBus::new(128),
            workspace.clone(),
        ));
        let root = runtime
            .create_thread(CreateThreadRequest {
                schedule: None,
                title: Some("root".into()),
                parent_id: None,
                agent: Some("build".into()),
                provider: Some("fabric".into()),
                model: Some("fixture".into()),
                ultracode: Some(false),
            })
            .await
            .unwrap();
        let host = WorkflowRuntimeAdapter::new();
        host.attach(&runtime);
        let run_thread = host
            .create_run_thread(Some(root.id), "Workflow · fixture")
            .await
            .unwrap();

        let fallback = host
            .resolve_agent_model(root.id, "missing/unavailable")
            .await
            .unwrap();
        assert_eq!(fallback.resolved, "fabric/fixture");
        assert_eq!(
            fallback.unavailable_requested.as_deref(),
            Some("missing/unavailable")
        );
        let available = host
            .resolve_agent_model(root.id, "fabric/fixture")
            .await
            .unwrap();
        assert_eq!(available.resolved, "fabric/fixture");
        assert!(available.unavailable_requested.is_none());

        let tool = host
            .request(
                run_thread,
                HostCall {
                    run_id: "fixture-run".into(),
                    request_id: 1,
                    method: HostRequestMethod::Tool,
                    params: json!({
                        "name": "read",
                        "args": {"path": "fixture.txt"},
                        "options": {}
                    }),
                    cancellation: CancellationToken::new(),
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(tool["output"], "host tool output");
        let run_items = runtime.items(run_thread, None).await.unwrap();
        assert!(run_items.iter().any(
            |item| matches!(item.data, ItemData::ToolCall { ref name, .. } if name == "read")
        ));
        assert!(run_items.iter().any(|item| matches!(item.data, ItemData::ToolResult { ref name, is_error: false, .. } if name == "read")));

        let reply = host
            .request(
                run_thread,
                HostCall {
                    run_id: "fixture-run".into(),
                    request_id: 2,
                    method: HostRequestMethod::Agent,
                    params: json!({
                        "prompt": "try the fabricated call",
                        "tools": {"bash": false}
                    }),
                    cancellation: CancellationToken::new(),
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reply["text"], "bounded agent completed");
        let child_id = reply["threadID"]
            .as_str()
            .and_then(|value| uuid::Uuid::parse_str(value).ok())
            .map(ThreadId)
            .expect("agent reply threadID");
        let child = runtime.get_thread(child_id).await.unwrap();
        assert_eq!(child.parent_id, Some(run_thread));
        assert_eq!(child.provider, "fabric");
        assert_eq!(child.model, "fixture");
        assert!(
            provider
                .advertised
                .lock()
                .unwrap()
                .iter()
                .all(|tools| !tools.iter().any(|name| name == "bash")),
            "disabled bash must never reach the provider schema"
        );
        let child_items = runtime.items(child_id, None).await.unwrap();
        assert!(child_items.iter().any(|item| {
            matches!(
                item.data,
                ItemData::ToolResult {
                    ref name,
                    ref output,
                    is_error: true,
                    ..
                } if name == "bash"
                    && output.contains("disabled")
                    && output.contains("Do not retry")
            )
        }));
        assert!(!workspace.join("should-not-exist").exists());

        host.request(
            run_thread,
            HostCall {
                run_id: "fixture-run".into(),
                request_id: 3,
                method: HostRequestMethod::AgentClose,
                params: json!({"token": reply["continuation"]}),
                cancellation: CancellationToken::new(),
            },
        )
        .await
        .unwrap();
        assert!(runtime.get_thread(child_id).await.unwrap().archived);
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    #[tokio::test]
    async fn workflow_build_agent_cannot_escape_its_plan_parent_write_ceiling() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-plan-host-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        let provider = Arc::new(FabricatingProvider {
            advertised: StdMutex::new(Vec::new()),
            tool_name: "write".into(),
            arguments: json!({"path": "must-not-exist.txt", "content": "escaped"}),
        });
        let providers = Arc::new(StaticProviderRegistry::new([
            provider.clone() as Arc<dyn ModelProvider>
        ]));
        let repository = Arc::new(SqliteStore::in_memory().unwrap());
        let tools = Arc::new(crate::NativeToolRegistry::new(workspace.clone()));
        let runtime = Arc::new(AgentRuntime::with_workspace(
            repository,
            providers,
            tools,
            EventBus::new(128),
            workspace.clone(),
        ));
        let root = runtime
            .create_thread(CreateThreadRequest {
                schedule: None,
                title: Some("plan root".into()),
                parent_id: None,
                agent: Some("plan".into()),
                provider: Some("fabric".into()),
                model: Some("fixture".into()),
                ultracode: Some(false),
            })
            .await
            .unwrap();
        let host = WorkflowRuntimeAdapter::new();
        host.attach(&runtime);
        let run_thread = host
            .create_run_thread(Some(root.id), "Workflow · plan boundary")
            .await
            .unwrap();

        let reply = host
            .request(
                run_thread,
                HostCall {
                    run_id: "plan-boundary-run".into(),
                    request_id: 1,
                    method: HostRequestMethod::Agent,
                    params: json!({
                        "prompt": "try to write outside the plan directory",
                        "agent": "build"
                    }),
                    cancellation: CancellationToken::new(),
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reply["text"], "bounded agent completed");
        let child_id = reply["threadID"]
            .as_str()
            .and_then(|value| uuid::Uuid::parse_str(value).ok())
            .map(ThreadId)
            .expect("agent reply threadID");
        assert_eq!(runtime.get_thread(child_id).await.unwrap().agent, "build");
        assert!(
            provider
                .advertised
                .lock()
                .unwrap()
                .iter()
                .any(|tools| tools.iter().any(|name| name == "write")),
            "the fixture must exercise permission denial, not schema filtering"
        );
        let child_items = runtime.items(child_id, None).await.unwrap();
        assert!(child_items.iter().any(|item| {
            matches!(
                item.data,
                ItemData::ToolResult {
                    ref name,
                    ref output,
                    is_error: true,
                    ..
                } if name == "write"
                    && output.contains("denied by permission rule 'edit:*'")
            )
        }));
        assert!(!workspace.join("must-not-exist.txt").exists());

        host.dispose_run_thread(run_thread).await.unwrap();
        assert!(runtime.get_thread(child_id).await.unwrap().archived);
        let followup = host
            .request(
                run_thread,
                HostCall {
                    run_id: "plan-boundary-run".into(),
                    request_id: 2,
                    method: HostRequestMethod::AgentFollowup,
                    params: json!({
                        "token": reply["continuation"],
                        "input": "should be closed"
                    }),
                    cancellation: CancellationToken::new(),
                },
            )
            .await
            .unwrap_err();
        assert!(followup.to_string().contains("closed"), "{followup}");

        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    #[tokio::test]
    async fn direct_workflow_tool_inherits_plan_ceiling_through_build_descendants() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-direct-ceiling-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        let provider = Arc::new(FabricatingProvider {
            advertised: StdMutex::new(Vec::new()),
            tool_name: "read".into(),
            arguments: json!({"path": "unused"}),
        });
        let providers = Arc::new(StaticProviderRegistry::new([
            provider as Arc<dyn ModelProvider>
        ]));
        let repository = Arc::new(SqliteStore::in_memory().unwrap());
        let tools = Arc::new(crate::NativeToolRegistry::new(workspace.clone()));
        let runtime = Arc::new(AgentRuntime::with_workspace(
            repository,
            providers,
            tools,
            EventBus::new(128),
            workspace.clone(),
        ));
        let root = runtime
            .create_thread(CreateThreadRequest {
                schedule: None,
                title: Some("plan root".into()),
                parent_id: None,
                agent: Some("plan".into()),
                provider: Some("fabric".into()),
                model: Some("fixture".into()),
                ultracode: Some(false),
            })
            .await
            .unwrap();
        let build_descendant = runtime
            .create_thread(CreateThreadRequest {
                schedule: None,
                title: Some("build descendant".into()),
                parent_id: Some(root.id),
                agent: Some("build".into()),
                provider: Some("fabric".into()),
                model: Some("fixture".into()),
                ultracode: Some(false),
            })
            .await
            .unwrap();
        let host = WorkflowRuntimeAdapter::new();
        host.attach(&runtime);
        let run_thread = host
            .create_run_thread(Some(build_descendant.id), "nested workflow")
            .await
            .unwrap();
        assert_eq!(runtime.get_thread(run_thread).await.unwrap().agent, "build");

        let error = host
            .request(
                run_thread,
                HostCall {
                    run_id: "nested-plan-run".into(),
                    request_id: 1,
                    method: HostRequestMethod::Tool,
                    params: json!({
                        "name": "write",
                        "args": {"path": "must-not-exist.txt", "content": "escaped"}
                    }),
                    cancellation: CancellationToken::new(),
                },
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("permission denied: edit:"));
        assert!(!workspace.join("must-not-exist.txt").exists());
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }

    #[tokio::test]
    async fn cancelling_direct_workflow_permission_removes_the_pending_prompt() {
        let workspace =
            std::env::temp_dir().join(format!("haarness-direct-cancel-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::write(workspace.join("fixture.txt"), "never read")
            .await
            .unwrap();
        let provider = Arc::new(FabricatingProvider {
            advertised: StdMutex::new(Vec::new()),
            tool_name: "read".into(),
            arguments: json!({"path": "fixture.txt"}),
        });
        let providers = Arc::new(StaticProviderRegistry::new([
            provider as Arc<dyn ModelProvider>
        ]));
        let repository = Arc::new(SqliteStore::in_memory().unwrap());
        let tools = Arc::new(crate::NativeToolRegistry::new(workspace.clone()));
        let runtime = Arc::new(AgentRuntime::with_config(
            repository,
            providers,
            tools,
            EventBus::new(128),
            workspace.clone(),
            String::new(),
            RuntimeConfig {
                base_rules: vec![PermissionRule {
                    pattern: "read:*".into(),
                    decision: PermissionDecision::Ask,
                }],
                ..RuntimeConfig::default()
            },
        ));
        let root = runtime
            .create_thread(CreateThreadRequest {
                schedule: None,
                title: Some("root".into()),
                parent_id: None,
                agent: Some("build".into()),
                provider: Some("fabric".into()),
                model: Some("fixture".into()),
                ultracode: Some(false),
            })
            .await
            .unwrap();
        let host = WorkflowRuntimeAdapter::new();
        host.attach(&runtime);
        let run_thread = host
            .create_run_thread(Some(root.id), "cancellable workflow")
            .await
            .unwrap();
        let cancellation = CancellationToken::new();
        let request_host = Arc::clone(&host);
        let request_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            request_host
                .request(
                    run_thread,
                    HostCall {
                        run_id: "cancel-run".into(),
                        request_id: 1,
                        method: HostRequestMethod::Tool,
                        params: json!({"name":"read", "args":{"path":"fixture.txt"}}),
                        cancellation: request_cancellation,
                    },
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if !runtime.pending_permissions().is_empty() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("direct tool raised a permission prompt");

        cancellation.cancel();
        let error = task.await.unwrap().unwrap_err();
        assert!(error.to_string().contains("cancelled"), "{error}");
        assert!(runtime.pending_permissions().is_empty());
        tokio::fs::remove_dir_all(workspace).await.unwrap();
    }
}
