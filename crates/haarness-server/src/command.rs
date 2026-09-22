//! Authoritative slash-command registry and dispatcher.
//!
//! Resolution is case-insensitive and deliberately ordered: the reserved
//! `/workflow` and `/goal` managers win first, configured prompt commands win
//! over direct workflow invocations, and discovered workflows fill the
//! remaining names.
//! Both `GET /v1/commands` and command execution consume this exact resolver.

use std::collections::HashSet;

use haarness_core::{CoreError, CoreResult};
use haarness_protocol::{
    CommandExecutionResponse, CommandInfo, ExecuteCommandRequest, GoalActionRequest,
    StartTurnRequest, ThreadId, WorkflowDefinition, WorkflowDialect, WorkflowId, WorkflowRun,
    WorkflowRunStatus, WorkflowStartRequest,
};
use haarness_workflow_catalog::WorkflowCatalog;
use serde_json::{Value, json};

use crate::api::ServerState;

#[derive(Clone)]
enum CommandKind {
    WorkflowManager,
    GoalManager,
    Configured,
    Workflow(Box<WorkflowDefinition>),
}

#[derive(Clone)]
struct ResolvedCommand {
    info: CommandInfo,
    kind: CommandKind,
}

/// Return only executable command winners, using the same resolver as the
/// POST endpoint. Hidden collision losers remain available through their
/// explicit owner (for example `/workflow run <name>` for a workflow shadowed
/// by a configured prompt command).
pub(crate) fn registry(state: &ServerState) -> Vec<CommandInfo> {
    resolved_commands(state)
        .into_iter()
        .map(|resolved| resolved.info)
        .collect()
}

pub(crate) async fn execute(
    state: &ServerState,
    thread_id: ThreadId,
    request: ExecuteCommandRequest,
) -> CoreResult<CommandExecutionResponse> {
    // Establish both existence and workspace ownership before dispatch. A
    // server instance owns exactly one workspace and its thread repository.
    state.runtime.get_thread(thread_id).await?;

    let name = normalize_requested_name(&request.name)?;
    let commands = resolved_commands(state);
    let resolved = commands
        .iter()
        .find(|candidate| candidate.info.name.eq_ignore_ascii_case(name))
        .cloned()
        .ok_or_else(|| unknown_command(name, &commands))?;

    match resolved.kind {
        CommandKind::WorkflowManager => execute_workflow_manager(state, thread_id, request).await,
        CommandKind::GoalManager => execute_goal_manager(state, thread_id, request).await,
        CommandKind::Configured => {
            execute_prompt_command(state, thread_id, resolved, request).await
        }
        CommandKind::Workflow(definition) => {
            execute_workflow_definition(state, thread_id, *definition, request).await
        }
    }
}

fn resolved_commands(state: &ServerState) -> Vec<ResolvedCommand> {
    let mut seen = HashSet::new();
    let mut commands = Vec::new();
    push_unique(
        &mut commands,
        &mut seen,
        ResolvedCommand {
            info: CommandInfo {
                execution: Default::default(),
                name: "workflow".into(),
                description: "Browse and manage durable workflows".into(),
                source: Some("builtin".into()),
                agent: None,
                model: None,
                argument_hint: Some(
                    "[list|run <name>|inspect|pause|resume|cancel|delete <run-id>]".into(),
                ),
                template: String::new(),
                plugin: None,
            },
            kind: CommandKind::WorkflowManager,
        },
    );
    push_unique(
        &mut commands,
        &mut seen,
        ResolvedCommand {
            info: CommandInfo {
                execution: Default::default(),
                name: "goal".into(),
                description: "Set or manage a persistent autonomous goal".into(),
                source: Some("builtin".into()),
                agent: None,
                model: None,
                argument_hint: Some(
                    "[objective|status|pause|resume|clear|complete|block|edit]".into(),
                ),
                template: String::new(),
                plugin: None,
            },
            kind: CommandKind::GoalManager,
        },
    );

    for configured in state.runtime.commands() {
        if !valid_command_name(&configured.name) {
            continue;
        }
        let mut info = configured.clone();
        info.source = Some("configured".into());
        push_unique(
            &mut commands,
            &mut seen,
            ResolvedCommand {
                info,
                kind: CommandKind::Configured,
            },
        );
    }

    let catalog = state.workflows.as_ref().map_or_else(
        || WorkflowCatalog::for_workspace(&state.workspace).list(),
        |coordinator| coordinator.catalog().list(),
    );
    let mut workflows = catalog.workflows;
    workflows.sort_by(|left, right| {
        left.invocation
            .to_ascii_lowercase()
            .cmp(&right.invocation.to_ascii_lowercase())
            .then_with(|| left.invocation.cmp(&right.invocation))
    });
    for workflow in workflows {
        if !valid_command_name(&workflow.invocation) {
            continue;
        }
        let info = workflow_command_info(&workflow);
        push_unique(
            &mut commands,
            &mut seen,
            ResolvedCommand {
                info,
                kind: CommandKind::Workflow(Box::new(workflow)),
            },
        );
    }

    commands.sort_by(|left, right| {
        left.info
            .name
            .to_ascii_lowercase()
            .cmp(&right.info.name.to_ascii_lowercase())
            .then_with(|| left.info.name.cmp(&right.info.name))
    });
    commands
}

async fn execute_goal_manager(
    state: &ServerState,
    thread_id: ThreadId,
    request: ExecuteCommandRequest,
) -> CoreResult<CommandExecutionResponse> {
    let raw = command_arguments_text(request.arguments.clone())?;
    let objective = raw.trim();
    let control = match objective.to_ascii_lowercase().as_str() {
        "" | "status" | "edit" => Some(GoalActionRequest::Status),
        "pause" => Some(GoalActionRequest::Pause),
        "resume" => Some(GoalActionRequest::Resume),
        "clear" => Some(GoalActionRequest::Clear),
        "complete" => Some(GoalActionRequest::Complete),
        "block" => Some(GoalActionRequest::Block { reason: None }),
        _ => None,
    };
    if let Some(action) = control {
        let response = state.runtime.goal_action(thread_id, action).await?;
        return Ok(CommandExecutionResponse::Text {
            text: response.message,
        });
    }

    // A reserved server command cannot atomically replace durable goal state
    // and steer an already-running turn while also honoring per-turn provider,
    // model, agent, reasoning, and tool overrides. Fail before the Set action
    // so callers never observe a partially-applied objective. Interactive
    // clients may use the thread input queue after updating goal state through
    // the typed goal endpoint, which preserves the active turn's configuration.
    if state.runtime.active_turn(thread_id).await?.is_some() {
        return Err(CoreError::Conflict(
            "cannot start /goal while a turn is running; queue it after the active turn or use the interactive goal manager"
                .into(),
        ));
    }

    state
        .runtime
        .goal_action(
            thread_id,
            GoalActionRequest::Set {
                goal: objective.to_owned(),
                token_budget: None,
            },
        )
        .await?;
    let turn = state
        .runtime
        .start_turn(
            thread_id,
            StartTurnRequest {
                request_id: request.request_id,
                text: objective.to_owned(),
                attachments: request.attachments,
                synthetic: false,
                auto_approve: false,
                agent: request.agent,
                provider: request.provider,
                model: request.model,
                reasoning_effort: request.reasoning_effort.or(request.variant),
                tools: request.tools,
                // The objective has already been resolved by the authoritative
                // server command registry. Re-running it through the runtime's
                // configured prompt-command resolver would reject the reserved
                // builtin as an unknown configured command.
                command: None,
            },
        )
        .await?;
    Ok(CommandExecutionResponse::Turn { turn })
}

fn push_unique(
    commands: &mut Vec<ResolvedCommand>,
    seen: &mut HashSet<String>,
    command: ResolvedCommand,
) {
    if seen.insert(command.info.name.to_ascii_lowercase()) {
        commands.push(command);
    }
}

fn workflow_command_info(workflow: &WorkflowDefinition) -> CommandInfo {
    let kind = match workflow.dialect {
        WorkflowDialect::Native => "native",
        WorkflowDialect::ClaudePortable => "portable",
    };
    let description = workflow
        .meta
        .description
        .as_deref()
        .or(workflow.meta.when_to_use.as_deref())
        .map_or_else(
            || format!("[{kind}] workflow"),
            |text| format!("[{kind}] {text}"),
        );
    let argument_hint = (!workflow.meta.arguments.is_empty()).then(|| {
        workflow
            .meta
            .arguments
            .keys()
            .map(|key| format!("<{key}>"))
            .collect::<Vec<_>>()
            .join(" ")
    });
    CommandInfo {
        execution: Default::default(),
        name: workflow.invocation.clone(),
        description,
        source: Some("workflow".into()),
        agent: None,
        model: None,
        argument_hint,
        template: String::new(),
        plugin: workflow.plugin_name.clone(),
    }
}

async fn execute_prompt_command(
    state: &ServerState,
    thread_id: ThreadId,
    resolved: ResolvedCommand,
    request: ExecuteCommandRequest,
) -> CoreResult<CommandExecutionResponse> {
    if request.variant.is_some() {
        return Err(CoreError::Invalid(
            "variant is only supported by workflow commands; use reasoningEffort for prompt commands"
                .into(),
        ));
    }
    let text = command_arguments_text(request.arguments)?;
    let turn = state
        .runtime
        .start_turn(
            thread_id,
            StartTurnRequest {
                request_id: request.request_id,
                text,
                attachments: request.attachments,
                synthetic: false,
                auto_approve: false,
                agent: request.agent,
                provider: request.provider,
                model: request.model,
                reasoning_effort: request.reasoning_effort,
                tools: request.tools,
                command: Some(resolved.info.name),
            },
        )
        .await?;
    Ok(CommandExecutionResponse::Turn { turn })
}

async fn execute_workflow_definition(
    state: &ServerState,
    thread_id: ThreadId,
    definition: WorkflowDefinition,
    request: ExecuteCommandRequest,
) -> CoreResult<CommandExecutionResponse> {
    let coordinator = workflow_coordinator(state)?;
    let model = workflow_model(request.provider.as_deref(), request.model.as_deref())?;
    let args = normalize_workflow_arguments(&definition, request.arguments)?;
    let run = coordinator
        .start(WorkflowStartRequest {
            name: Some(definition.invocation),
            args,
            agent: request.agent,
            model,
            variant: request.variant.or(request.reasoning_effort),
            thread_id: Some(thread_id),
            wait: request.wait,
            timeout_ms: request.timeout_ms,
            ..WorkflowStartRequest::default()
        })
        .await?;
    Ok(CommandExecutionResponse::WorkflowRun { run: Box::new(run) })
}

async fn execute_workflow_manager(
    state: &ServerState,
    thread_id: ThreadId,
    request: ExecuteCommandRequest,
) -> CoreResult<CommandExecutionResponse> {
    let raw = command_arguments_text(request.arguments.clone())?;
    let raw = raw.trim();
    if raw.is_empty()
        || raw.eq_ignore_ascii_case("list")
        || raw.eq_ignore_ascii_case("ls")
        || raw.eq_ignore_ascii_case("status")
    {
        return workflow_summary(state, thread_id).await;
    }
    let (action, tail) = split_head(raw);
    match action.to_ascii_lowercase().as_str() {
        "run" => {
            let (name, arguments) = split_head(tail);
            if name.is_empty() {
                return Err(CoreError::Invalid(
                    "usage: /workflow run <name> [plain text or JSON args]".into(),
                ));
            }
            execute_named_workflow(state, thread_id, name, arguments, request).await
        }
        "inspect" | "show" | "get" | "status" => {
            let id = required_workflow_id(tail)?;
            let run = owned_thread_run(state, thread_id, id).await?;
            Ok(CommandExecutionResponse::WorkflowRun { run: Box::new(run) })
        }
        "cancel" => {
            let id = required_workflow_id(tail)?;
            owned_thread_run(state, thread_id, id).await?;
            let run = workflow_coordinator(state)?.cancel(id).await?;
            Ok(CommandExecutionResponse::WorkflowRun { run: Box::new(run) })
        }
        "pause" => {
            let id = required_workflow_id(tail)?;
            owned_thread_run(state, thread_id, id).await?;
            let run = workflow_coordinator(state)?.pause(id).await?;
            Ok(CommandExecutionResponse::WorkflowRun { run: Box::new(run) })
        }
        "resume" => {
            let id = required_workflow_id(tail)?;
            owned_thread_run(state, thread_id, id).await?;
            let run = workflow_coordinator(state)?.resume(id).await?;
            Ok(CommandExecutionResponse::WorkflowRun { run: Box::new(run) })
        }
        "delete" | "rm" => {
            let id = required_workflow_id(tail)?;
            let run = owned_thread_run(state, thread_id, id).await?;
            if !workflow_coordinator(state)?.delete(id).await? {
                return Err(CoreError::NotFound(format!("workflow run {id}")));
            }
            Ok(CommandExecutionResponse::Text {
                text: format!("Deleted workflow {} · {id}", run.workflow_name),
            })
        }
        _ => execute_named_workflow(state, thread_id, action, tail, request).await,
    }
}

async fn execute_named_workflow(
    state: &ServerState,
    thread_id: ThreadId,
    name: &str,
    argument_tail: &str,
    mut request: ExecuteCommandRequest,
) -> CoreResult<CommandExecutionResponse> {
    let definition = workflow_definition(state, name)?;
    request.arguments = parse_argument_tail(argument_tail)?;
    execute_workflow_definition(state, thread_id, definition, request).await
}

async fn workflow_summary(
    state: &ServerState,
    thread_id: ThreadId,
) -> CoreResult<CommandExecutionResponse> {
    let catalog = state.workflows.as_ref().map_or_else(
        || WorkflowCatalog::for_workspace(&state.workspace).list(),
        |coordinator| coordinator.catalog().list(),
    );
    let mut lines = vec![format!("Workflows ({})", catalog.workflows.len())];
    for definition in catalog.workflows.iter().take(20) {
        let description = definition
            .meta
            .description
            .as_deref()
            .or(definition.meta.when_to_use.as_deref())
            .unwrap_or("reusable workflow");
        lines.push(format!("/{} · {description}", definition.invocation));
    }
    if let Some(coordinator) = &state.workflows {
        let mut runs = coordinator.list(None, 100).await?;
        runs.retain(|run| run.definition.launch_thread_id == Some(thread_id));
        if !runs.is_empty() {
            lines.push("Recent runs".into());
            for run in runs.into_iter().take(10) {
                lines.push(format!(
                    "{} · {} · {}",
                    run.id,
                    run.workflow_name,
                    status_name(run.status)
                ));
            }
        }
    }
    if !catalog.invalid.is_empty() {
        lines.push(format!("{} invalid definition(s)", catalog.invalid.len()));
    }
    Ok(CommandExecutionResponse::Text {
        text: lines.join("\n"),
    })
}

fn workflow_definition(state: &ServerState, name: &str) -> CoreResult<WorkflowDefinition> {
    let catalog = state.workflows.as_ref().map_or_else(
        || WorkflowCatalog::for_workspace(&state.workspace).list(),
        |coordinator| coordinator.catalog().list(),
    );
    catalog
        .workflows
        .iter()
        .find(|workflow| workflow.invocation.eq_ignore_ascii_case(name))
        .cloned()
        .ok_or_else(|| {
            let available = catalog
                .workflows
                .iter()
                .take(8)
                .map(|workflow| format!("/{}", workflow.invocation))
                .collect::<Vec<_>>()
                .join(", ");
            CoreError::NotFound(if available.is_empty() {
                format!("workflow /{name}; no workflows are installed")
            } else {
                format!("workflow /{name}; available: {available}")
            })
        })
}

async fn owned_thread_run(
    state: &ServerState,
    thread_id: ThreadId,
    id: WorkflowId,
) -> CoreResult<WorkflowRun> {
    let run = workflow_coordinator(state)?.get(id, None).await?;
    if run.definition.launch_thread_id != Some(thread_id) && run.thread_id != Some(thread_id) {
        return Err(CoreError::NotFound(format!(
            "workflow run {id} in thread {thread_id}"
        )));
    }
    Ok(run)
}

fn workflow_coordinator(
    state: &ServerState,
) -> CoreResult<std::sync::Arc<haarness_workflows::WorkflowCoordinator>> {
    state
        .workflows
        .clone()
        .ok_or_else(|| CoreError::Invalid("workflow execution is unavailable".into()))
}

fn workflow_model(provider: Option<&str>, model: Option<&str>) -> CoreResult<Option<String>> {
    match (provider.filter(|value| !value.trim().is_empty()), model) {
        (_, Some(model)) if model.contains('/') => Ok(Some(model.to_owned())),
        (Some(provider), Some(model)) if !model.trim().is_empty() => {
            Ok(Some(format!("{provider}/{model}")))
        }
        (None, Some(model)) if !model.trim().is_empty() => Ok(Some(model.to_owned())),
        (Some(_), None) => Err(CoreError::Invalid(
            "provider requires a model for workflow commands".into(),
        )),
        _ => Ok(None),
    }
}

fn normalize_workflow_arguments(
    definition: &WorkflowDefinition,
    arguments: Option<Value>,
) -> CoreResult<Option<Value>> {
    match definition.dialect {
        WorkflowDialect::ClaudePortable => match arguments {
            None => Ok(None),
            Some(Value::String(raw)) => Ok(Some(
                serde_json::from_str(&raw).unwrap_or_else(|_| json!({"input": raw})),
            )),
            Some(value) => Ok(Some(value)),
        },
        WorkflowDialect::Native => match arguments {
            None => Ok(Some(json!({}))),
            Some(value @ Value::Object(_)) => Ok(Some(value)),
            Some(Value::Array(_)) => Err(CoreError::Invalid(
                "native workflow arguments must be a JSON object".into(),
            )),
            Some(Value::String(raw)) => Ok(Some(json!({"input": raw}))),
            Some(value) => Ok(Some(json!({"input": value}))),
        },
    }
}

fn parse_argument_tail(tail: &str) -> CoreResult<Option<Value>> {
    let tail = tail.trim();
    if tail.is_empty() {
        return Ok(None);
    }
    if tail.starts_with('{') || tail.starts_with('[') {
        return serde_json::from_str(tail).map(Some).map_err(|error| {
            CoreError::Invalid(format!("invalid workflow JSON arguments: {error}"))
        });
    }
    Ok(Some(Value::String(tail.to_owned())))
}

fn command_arguments_text(arguments: Option<Value>) -> CoreResult<String> {
    match arguments {
        None => Ok(String::new()),
        Some(Value::String(value)) => Ok(value),
        Some(value) => serde_json::to_string(&value)
            .map_err(|error| CoreError::Invalid(format!("invalid command arguments: {error}"))),
    }
}

fn normalize_requested_name(name: &str) -> CoreResult<&str> {
    let name = name.trim().trim_start_matches('/');
    if !valid_command_name(name) {
        return Err(CoreError::Invalid(
            "command name must be one non-empty slash-command token".into(),
        ));
    }
    Ok(name)
}

fn valid_command_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(char::is_whitespace) && !name.contains('/')
}

fn split_head(value: &str) -> (&str, &str) {
    value
        .trim()
        .split_once(char::is_whitespace)
        .map_or((value.trim(), ""), |(head, tail)| (head, tail.trim()))
}

fn required_workflow_id(value: &str) -> CoreResult<WorkflowId> {
    let value = value.trim();
    if value.is_empty() || value.contains(char::is_whitespace) {
        return Err(CoreError::Invalid(
            "a single workflow run ID is required".into(),
        ));
    }
    uuid::Uuid::parse_str(value)
        .map(WorkflowId)
        .map_err(|error| CoreError::Invalid(format!("invalid workflow run ID: {error}")))
}

fn unknown_command(name: &str, commands: &[ResolvedCommand]) -> CoreError {
    let available = commands
        .iter()
        .take(12)
        .map(|command| format!("/{}", command.info.name))
        .collect::<Vec<_>>()
        .join(", ");
    CoreError::NotFound(format!("command /{name}; available: {available}"))
}

fn status_name(status: WorkflowRunStatus) -> &'static str {
    match status {
        WorkflowRunStatus::Running => "running",
        WorkflowRunStatus::Completed => "completed",
        WorkflowRunStatus::Failed => "failed",
        WorkflowRunStatus::Cancelled => "cancelled",
        WorkflowRunStatus::Paused => "paused",
        WorkflowRunStatus::Interrupted => "interrupted",
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_requested_name, parse_argument_tail, workflow_model};

    #[test]
    fn command_names_and_argument_tails_are_lossless() {
        assert_eq!(normalize_requested_name(" /ReLeAsE ").unwrap(), "ReLeAsE");
        assert!(normalize_requested_name("two words").is_err());
        assert_eq!(
            parse_argument_tail(r#"{"dryRun":true}"#).unwrap(),
            Some(serde_json::json!({"dryRun": true}))
        );
        assert_eq!(
            parse_argument_tail("ship exactly this").unwrap(),
            Some(serde_json::Value::String("ship exactly this".into()))
        );
    }

    #[test]
    fn provider_and_model_propagate_as_one_workflow_model() {
        assert_eq!(
            workflow_model(Some("openai"), Some("gpt-5.6")).unwrap(),
            Some("openai/gpt-5.6".into())
        );
        assert_eq!(
            workflow_model(Some("ignored"), Some("anthropic/claude-opus-5")).unwrap(),
            Some("anthropic/claude-opus-5".into())
        );
        assert!(workflow_model(Some("openai"), None).is_err());
    }
}
