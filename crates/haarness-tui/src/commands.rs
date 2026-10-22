//! Slash-command catalog and execution.

use anyhow::Result;
use haarness_protocol::{
    CommandExecutionResponse, CompactRequest, CreateThreadRequest, ExecuteCommandRequest,
    GoalActionRequest, GoalActionResponse, GoalState, GoalStatus, PtyCreateRequest, RequestId,
    StartTurnRequest, Thread, UpdateThreadRequest, WorkflowId, WorkflowRun, WorkflowStartRequest,
    WorkspaceCleanupRequest, WorkspaceCreateRequest, WorkspaceInfo, WorkspaceListResponse,
    WorkspaceOperationResponse, WorkspaceRefKind, WorkspaceRefsResponse, WorkspaceRemoveRequest,
    WorkspaceResetRequest, WorkspaceResolveRefRequest, WorkspaceResolveRefResponse,
};

use crate::api::ApiClient;
use crate::input::{AppAction, WorkspaceCreateRetry, WorkspaceMutation, WorkspaceRefOperation};
use crate::state::{App, OptionEntry, OptionsKind, Overlay, OverlayChoice, ToastVariant};

/// The server query for `/stats` and `/insights`, and whether `/insights`
/// should open its report (`--open`, as `haarness insights` takes).
pub(crate) fn analytics_query(
    arguments: &str,
    report: bool,
) -> Result<(Vec<(String, String)>, bool)> {
    let mut query = Vec::new();
    let mut open = false;
    let mut words = arguments.split_whitespace();
    while let Some(word) = words.next() {
        let (key, value) = match word {
            "--current-project" => ("currentProject", "true".to_owned()),
            "--days" | "--limit" | "--project" => {
                let value = words
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("{word} requires a value"))?;
                (word.trim_start_matches('-'), value.to_owned())
            }
            "--open" if report => {
                open = true;
                continue;
            }
            "--no-open" if report => {
                open = false;
                continue;
            }
            "--no-llm" => continue,
            _ if report => anyhow::bail!(
                "Unknown option {word}; use --days N, --current-project, --project ID, --limit N or --open"
            ),
            _ => anyhow::bail!(
                "Unknown option {word}; use --days N, --current-project, --project ID or --limit N"
            ),
        };
        query.push((key.to_owned(), value));
    }
    Ok((query, open))
}

pub(crate) struct WorkspaceMutationCompletion {
    pub operation: WorkspaceMutation,
    pub result: std::result::Result<WorkspaceOperationResponse, String>,
    pub catalog: Option<WorkspaceListResponse>,
}

/// A review the background worker finished loading. It carries the request it
/// answers so a reader who switched source, changed context or pressed Escape
/// meanwhile is never handed a stale review.
pub(crate) struct DiffLoadCompletion {
    pub source: crate::diffview::DiffSource,
    pub context_step: usize,
    pub result: std::result::Result<crate::api::Review, String>,
}

/// A review load running off the event loop, with the handle that cancels it.
pub(crate) struct DiffLoad {
    receiver: tokio::sync::oneshot::Receiver<DiffLoadCompletion>,
    task: tokio::task::JoinHandle<()>,
}

impl DiffLoad {
    /// Stop a load whose answer nobody is waiting for any more.
    pub(crate) fn cancel(self) {
        self.task.abort();
    }
}

/// Fetch a review away from the terminal event loop. `/diff` on a large
/// repository is seconds of server-side Git work; awaiting it inline froze
/// the whole TUI — no repaint, no SSE, no Escape — until it finished.
pub(crate) async fn run_diff_load(
    client: ApiClient,
    thread: haarness_protocol::ThreadId,
    source: crate::diffview::DiffSource,
    context_step: usize,
) -> DiffLoadCompletion {
    let result = match source.query() {
        // Only Git patches are cut server-side. Snapshot bodies arrive whole
        // and are re-cut locally whenever the context step changes.
        Some(mode) => {
            let context = crate::diffview::CONTEXT_STEPS
                [context_step.min(crate::diffview::CONTEXT_STEPS.len() - 1)];
            client.vcs_diff_with_context(mode, context).await
        }
        None => client.last_turn_diff(thread).await,
    }
    .map_err(|error| diff_error(&error));
    DiffLoadCompletion {
        source,
        context_step,
        result,
    }
}

/// A failed review in words that say what to do. A folder that is not a Git
/// repository has no working-tree review, but its last turn still has one. A
/// partial clone missing files the review needs gets them with
/// `git backfill`: the review never downloads them itself.
pub(crate) fn diff_error(error: &anyhow::Error) -> String {
    let text = format!("{error:#}");
    if text.contains("not inside a Git worktree") {
        return "This folder isn’t a Git repository · /diff last shows the last turn’s changes"
            .into();
    }
    if text.contains("has not downloaded the files the review needs") {
        return "Review needs files this clone hasn’t downloaded yet · project configuration isn’t trusted; run git backfill (Git 2.49+), then /diff again".into();
    }
    format!("Diff failed: {text}")
}

/// `/undo` and `/redo` when there is nothing to take back or bring back say
/// so, instead of quoting the server's reason for refusing.
pub(crate) fn undo_error(error: &anyhow::Error, action: &str) -> String {
    let text = error.to_string();
    match action {
        "Undo" if text.contains("snapshot to revert to") => "Nothing to undo".into(),
        "Redo" if text.contains("is not reverted") => "Nothing to redo".into(),
        _ => format!("{action} failed: {error:#}"),
    }
}

pub(crate) enum WorkspaceRefOperationResult {
    Loaded(WorkspaceRefsResponse),
    Resolved(WorkspaceResolveRefResponse),
}

pub(crate) struct WorkspaceRefOperationCompletion {
    pub operation: WorkspaceRefOperation,
    pub result: std::result::Result<WorkspaceRefOperationResult, String>,
}

fn workspace_default_display(refs: &WorkspaceRefsResponse) -> &str {
    refs.refs
        .iter()
        .find(|reference| reference.current && reference.reference == refs.default_ref)
        .or_else(|| refs.refs.iter().find(|reference| reference.current))
        .map_or_else(
            || {
                refs.default_ref
                    .strip_prefix("refs/heads/")
                    .unwrap_or(refs.default_ref.as_str())
            },
            |reference| reference.name.as_str(),
        )
}

/// Discover and validate create bases without awaiting Git reads on the
/// terminal event loop. The completion reducer owns all modal transitions.
pub(crate) async fn run_workspace_ref_operation(
    client: ApiClient,
    operation: WorkspaceRefOperation,
) -> WorkspaceRefOperationCompletion {
    let result = match &operation {
        WorkspaceRefOperation::Load => client
            .workspace_refs()
            .await
            .map(WorkspaceRefOperationResult::Loaded),
        WorkspaceRefOperation::Resolve { reference } => client
            .resolve_workspace_ref(&WorkspaceResolveRefRequest {
                reference: Some(reference.clone()),
            })
            .await
            .map(WorkspaceRefOperationResult::Resolved),
    }
    .map_err(|error| error.to_string());
    WorkspaceRefOperationCompletion { operation, result }
}

/// Run slow local-Git workspace work away from the terminal event loop. A
/// successful mutation also refreshes the catalog on the same worker task so
/// completion handling remains synchronous and rendering never stalls.
pub(crate) async fn run_workspace_mutation(
    client: ApiClient,
    operation: WorkspaceMutation,
) -> WorkspaceMutationCompletion {
    let result = match &operation {
        WorkspaceMutation::Open { reference } => client.open_workspace(reference).await,
        WorkspaceMutation::Create { request, .. } => client.create_workspace(request).await,
        WorkspaceMutation::Reset {
            reference, request, ..
        } => client.reset_workspace(reference, request).await,
        WorkspaceMutation::Remove {
            reference, request, ..
        } => client.remove_workspace(reference, request).await,
        WorkspaceMutation::Cleanup {
            reference, request, ..
        } => client.cleanup_workspace(reference, request).await,
    }
    .map_err(|error| error.to_string());
    let refresh = !matches!(&operation, WorkspaceMutation::Open { .. });
    let catalog = if result.is_ok() && refresh {
        client.workspaces().await.ok()
    } else {
        None
    };
    WorkspaceMutationCompletion {
        operation,
        result,
        catalog,
    }
}

#[derive(Clone, Copy)]
pub struct CommandSpec {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
}

pub const COMMANDS: &[CommandSpec] = &[
    command("new", &["clear"], "Start a new thread"),
    command(
        "sessions",
        &["threads", "resume", "continue"],
        "Resume a session · → actions · --all includes archived · also /resume",
    ),
    command(
        "worktree",
        &[
            "worktrees",
            "workspace",
            "workspaces",
            "project",
            "projects",
        ],
        "Browse project folders and worktrees · create/reset/remove with explicit flags",
    ),
    command("models", &["model", "mo"], "Switch the active model"),
    command(
        "connect",
        &["login"],
        "Connect a provider · sign in or add an API key · /connect [provider]",
    ),
    command(
        "providers",
        &["provider"],
        "Manage provider connections and custom endpoints",
    ),
    command("agents", &["agent"], "Switch the active agent"),
    command("plan", &[], "Use the plan agent"),
    command("review", &[], "Use the review agent"),
    command("build", &[], "Use the build agent"),
    command("explore", &[], "Use the explore agent"),
    command("compact", &[], "Create a durable compaction checkpoint"),
    command(
        "diff",
        &[],
        "Full-screen review: /diff [git|branch|last] · tree, split view, comments",
    ),
    command(
        "terminal",
        &["term"],
        "Open persistent terminal tabs · /terminal new creates another",
    ),
    command("undo", &[], "Stop active work and undo the latest change"),
    command("redo", &[], "Restore files undone by /undo"),
    command(
        "timeline",
        &["history", "checkpoints"],
        "Undo back to any earlier checkpoint in this session",
    ),
    command("fork", &[], "Fork this thread"),
    command(
        "messages",
        &["message", "browse"],
        "Search prompts and replies · jump, read, copy or fork · Alt+M",
    ),
    command(
        "rename",
        &[],
        "Rename this session: /rename [session-id] <title>",
    ),
    command("archive", &[], "Archive this thread and start a new one"),
    command(
        "reasoning",
        &["think", "thinking"],
        "Set effort or Ultracode orchestration: /reasoning high|xhigh|max|ultracode|off",
    ),
    command(
        "ultracode",
        &["uc"],
        "Toggle durable workflow orchestration for this session",
    ),
    command(
        "workflow",
        &["workflows", "wf"],
        "Browse/run workflows · /workflow <name> [args] · inspect/pause/resume/cancel/delete",
    ),
    command(
        "btw",
        &[],
        "Ask a read-only side question without changing this transcript",
    ),
    command("themes", &["theme"], "Switch the color theme"),
    command(
        "stats",
        &[],
        "Cross-project analytics: --days N --current-project --project ID --limit N",
    ),
    command(
        "insights",
        &[],
        "Save an HTML insights report: --days N --current-project --project ID --limit N --open",
    ),
    command(
        "language",
        &["lang", "locale"],
        "Switch the display language",
    ),
    command(
        "keybinds",
        &["shortcuts"],
        "Configure application shortcuts",
    ),
    command(
        "queue",
        &[],
        "Manage deferred messages · /queue <text> · Alt+Enter queues",
    ),
    command(
        "input-mode",
        &[],
        "Toggle project Steer/Queue · /input-mode [steer|queue|toggle|reset]",
    ),
    command("stash", &[], "Save and restore unsent prompts"),
    command(
        "usage",
        &["context"],
        "Inspect session usage, cache and context",
    ),
    command("servers", &["server"], "Manage servers and their projects"),
    command(
        "share",
        &[],
        "Review, share and revoke a conversation snapshot",
    ),
    command(
        "remote-control",
        &["rc", "remote"],
        "Continue this chat from your phone or any browser · /remote-control stop",
    ),
    command("chrome", &["browser"], "Use your Chrome with Haarness"),
    command("mcp", &["mcps"], "Manage MCP connections"),
    command("schedules", &["schedule"], "Manage scheduled runs"),
    command("hooks", &[], "Manage runtime hooks"),
    command("memory", &[], "Manage project memory"),
    command(
        "usage-statistics",
        &["telemetry"],
        "Share or stop sharing usage statistics",
    ),
    command(
        "profiles",
        &["profile"],
        "Save and restore configuration profiles",
    ),
    command(
        "marketplace",
        &["market"],
        "Browse, install and manage Haarness marketplace resources",
    ),
    command(
        "plugin",
        &["plugins"],
        "Manage and inspect Haarness plugins",
    ),
    command("skills", &[], "Show discovered skills"),
    command(
        "tools",
        &["details"],
        "Browse/toggle tools · /tools lazy auto|always|off · /tools show|hide",
    ),
    command("status", &[], "Show server and thread status"),
    command(
        "router",
        &["routers"],
        "Choose the Haarness Cloud router used on this computer",
    ),
    command(
        "billing",
        &["credits"],
        "Show Cloud plan and credits · /billing open|topup|usage",
    ),
    command(
        "tasks",
        &["subagents", "children"],
        "Open a running child agent's live view",
    ),
    command(
        "background",
        &["bg"],
        "Move foreground subagents to background: /background [child-id]",
    ),
    command(
        "goal",
        &[],
        "Persistent pursuit: /goal [objective|status|edit|pause|resume|clear|complete|block]",
    ),
    command(
        "loop",
        &["look"],
        "Recurring prompt: /loop <interval> <prompt> | /loop stop",
    ),
    command("copy", &[], "Copy the last reply to the clipboard"),
    command(
        "export",
        &[],
        "Write this thread's transcript to a markdown file",
    ),
    command("editor", &["edit"], "Compose the prompt in $EDITOR"),
    command("cancel", &["stop"], "Interrupt the active turn"),
    command(
        "permissions-mode",
        &["permission-mode"],
        "Switch approvals: /permissions-mode [review|auto] · ctrl+y",
    ),
    command("help", &["commands", "?"], "Show available commands"),
    command("exit", &["quit", "q"], "Exit Haarness"),
];

const fn command(
    name: &'static str,
    aliases: &'static [&'static str],
    description: &'static str,
) -> CommandSpec {
    CommandSpec {
        name,
        aliases,
        description,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum WorkflowSlashCommand {
    Dashboard,
    Run {
        name: String,
        args: Option<serde_json::Value>,
    },
    Inspect(String),
    Cancel(String),
    Pause(String),
    Resume(String),
    Delete(String),
}

/// Parse `/workflow` without losing the user's plain-text argument tail.
/// Valid JSON remains typed; non-JSON text is delivered as one string. A
/// malformed object/array is called out rather than silently changing type.
pub(crate) fn parse_workflow_command(arguments: &str) -> Result<WorkflowSlashCommand, String> {
    let arguments = arguments.trim();
    if arguments.is_empty()
        || arguments.eq_ignore_ascii_case("list")
        || arguments.eq_ignore_ascii_case("ls")
        || arguments.eq_ignore_ascii_case("status")
    {
        return Ok(WorkflowSlashCommand::Dashboard);
    }
    let (head, tail) = arguments
        .split_once(char::is_whitespace)
        .map_or((arguments, ""), |(head, tail)| (head, tail.trim()));
    match head.to_ascii_lowercase().as_str() {
        "list" => Err("Usage: /workflow list".into()),
        "run" => {
            let (name, raw_args) = tail
                .split_once(char::is_whitespace)
                .map_or((tail, ""), |(name, args)| (name, args.trim()));
            if name.is_empty() {
                return Err("Usage: /workflow run <name> [plain text or JSON args]".into());
            }
            Ok(WorkflowSlashCommand::Run {
                name: name.to_owned(),
                args: parse_workflow_args(raw_args)?,
            })
        }
        "inspect" | "show" | "get" | "status" => {
            required_workflow_id(tail).map(WorkflowSlashCommand::Inspect)
        }
        "cancel" => required_workflow_id(tail).map(WorkflowSlashCommand::Cancel),
        "pause" => required_workflow_id(tail).map(WorkflowSlashCommand::Pause),
        "resume" => required_workflow_id(tail).map(WorkflowSlashCommand::Resume),
        "delete" | "rm" => required_workflow_id(tail).map(WorkflowSlashCommand::Delete),
        _ => Ok(WorkflowSlashCommand::Run {
            name: head.to_owned(),
            args: parse_workflow_args(tail)?,
        }),
    }
}

fn required_workflow_id(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.contains(char::is_whitespace) {
        Err("A single workflow run ID is required".into())
    } else {
        Ok(value.to_owned())
    }
}

fn parse_workflow_args(value: &str) -> Result<Option<serde_json::Value>, String> {
    if value.is_empty() {
        return Ok(None);
    }
    if value.starts_with('{') || value.starts_with('[') {
        serde_json::from_str(value)
            .map(Some)
            .map_err(|error| format!("Invalid workflow JSON arguments: {error}"))
    } else {
        // Keep unbracketed input lossless here. The selected definition's
        // dialect decides whether `true`, `42`, and `null` are native text or
        // portable JSON primitives immediately before launch.
        Ok(Some(serde_json::Value::String(value.to_owned())))
    }
}

fn parse_workspace_create(value: &str) -> Result<WorkspaceCreateRequest, String> {
    let mut words = value.split_whitespace();
    let name = words.next().ok_or_else(|| {
        "Usage: /worktree create <name> [--from REF] [--branch NAME] [--path PATH]".to_owned()
    })?;
    let mut request = WorkspaceCreateRequest {
        name: name.to_owned(),
        ..WorkspaceCreateRequest::default()
    };
    while let Some(flag) = words.next() {
        let target = match flag {
            "--from" => &mut request.from_ref,
            "--branch" => &mut request.branch,
            "--path" => &mut request.path,
            _ => return Err(format!("Unknown worktree create option: {flag}")),
        };
        *target = Some(
            words
                .next()
                .ok_or_else(|| format!("{flag} requires a value"))?
                .to_owned(),
        );
    }
    Ok(request)
}

fn parse_workspace_reset(value: &str) -> Result<(String, WorkspaceResetRequest), String> {
    let mut words = value.split_whitespace();
    let reference = words
        .next()
        .ok_or_else(|| {
            "Usage: /worktree reset <name-or-id> --confirm [--discard-changes] [--remove-untracked] [--to REF]"
                .to_owned()
        })?
        .to_owned();
    let mut request = WorkspaceResetRequest::default();
    while let Some(flag) = words.next() {
        match flag {
            "--confirm" => request.confirm = true,
            "--discard-changes" => request.discard_changes = true,
            "--remove-untracked" => request.remove_untracked = true,
            "--to" => {
                request.to_ref = Some(
                    words
                        .next()
                        .ok_or_else(|| "--to requires a value".to_owned())?
                        .to_owned(),
                );
            }
            _ => return Err(format!("Unknown worktree reset option: {flag}")),
        }
    }
    Ok((reference, request))
}

fn parse_workspace_remove(value: &str) -> Result<(String, WorkspaceRemoveRequest), String> {
    let mut words = value.split_whitespace();
    let reference = words
        .next()
        .ok_or_else(|| {
            "Usage: /worktree remove <name-or-id> --confirm [--discard-changes] [--delete-branch]"
                .to_owned()
        })?
        .to_owned();
    let mut request = WorkspaceRemoveRequest::default();
    for flag in words {
        match flag {
            "--confirm" => request.confirm = true,
            "--discard-changes" => request.discard_changes = true,
            "--delete-branch" => request.delete_branch = true,
            _ => return Err(format!("Unknown worktree remove option: {flag}")),
        }
    }
    Ok((reference, request))
}

fn workflow_model(provider: &str, model: &str) -> String {
    if model.contains('/') {
        model.to_owned()
    } else {
        format!("{provider}/{model}")
    }
}

pub(crate) fn workflow_command_args(
    definition: &haarness_protocol::WorkflowDefinition,
    args: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    use haarness_protocol::WorkflowDialect;

    match definition.dialect {
        WorkflowDialect::ClaudePortable => {
            match args {
                None => Ok(serde_json::json!({})),
                Some(serde_json::Value::String(raw)) => Ok(serde_json::from_str(&raw)
                    .unwrap_or_else(|_| serde_json::json!({ "input": raw }))),
                Some(value) => Ok(value),
            }
        }
        WorkflowDialect::Native => match args {
            None => Ok(serde_json::json!({})),
            Some(value @ serde_json::Value::Object(_)) => Ok(value),
            Some(serde_json::Value::Array(_)) => {
                Err("Workflow JSON arguments must be an object".into())
            }
            Some(serde_json::Value::String(raw)) => Ok(serde_json::json!({ "input": raw })),
            Some(value) => Ok(serde_json::json!({ "input": value.to_string() })),
        },
    }
}

/// Edits between two names, counting a swapped pair of neighbours as one:
/// `/stauts` is one slip away from `/status`.
pub(crate) fn edit_distance(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let mut rows = vec![vec![0; right.len() + 1]; left.len() + 1];
    for (index, row) in rows.iter_mut().enumerate() {
        row[0] = index;
    }
    for (index, cell) in rows[0].iter_mut().enumerate() {
        *cell = index;
    }
    for i in 1..=left.len() {
        for j in 1..=right.len() {
            let cost = usize::from(left[i - 1] != right[j - 1]);
            let mut best = (rows[i - 1][j] + 1)
                .min(rows[i][j - 1] + 1)
                .min(rows[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && left[i - 1] == right[j - 2] && left[i - 2] == right[j - 1] {
                best = best.min(rows[i - 2][j - 2] + 1);
            }
            rows[i][j] = best;
        }
    }
    rows[left.len()][right.len()]
}

/// The `s` in "2 steps".
const fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

impl App {
    /// The undoable checkpoints in this session, newest first, each labelled
    /// by the request that produced it. Stepping back one change at a time is
    /// fine for the last thing you did; this is for "put it back the way it
    /// was before I asked for any of this".
    pub fn open_timeline_overlay(&mut self) {
        let mut boundaries: Vec<u64> = self
            .items
            .iter()
            .filter_map(|item| match &item.data {
                haarness_protocol::ItemData::Snapshot {
                    boundary_sequence: Some(boundary),
                    base_snapshot_id: Some(_),
                    ..
                } => Some(*boundary),
                _ => None,
            })
            .collect();
        boundaries.sort_unstable();
        boundaries.dedup();
        if boundaries.is_empty() {
            self.notice = "No folder checkpoints in this session yet".into();
            return;
        }
        let pending = self.thread.revert.as_ref().map(|state| state.sequence);
        let entries: Vec<OptionEntry> = boundaries
            .iter()
            .rev()
            .map(|boundary| {
                // The checkpoint guards the turn that starts just after it, so
                // name it with the request that opened that turn.
                let prompt = self
                    .items
                    .iter()
                    .find(|item| {
                        item.sequence > *boundary
                            && matches!(
                                item.data,
                                haarness_protocol::ItemData::UserMessage {
                                    synthetic: false,
                                    ..
                                }
                            )
                    })
                    .and_then(|item| match &item.data {
                        haarness_protocol::ItemData::UserMessage { text, .. } => Some(
                            crate::prompt_context::display(text)
                                .lines()
                                .find(|line| !line.trim().is_empty())
                                .unwrap_or_default()
                                .trim()
                                .to_owned(),
                        ),
                        _ => None,
                    })
                    .filter(|text| !text.is_empty())
                    .unwrap_or_else(|| format!("checkpoint at {boundary}"));
                let state = match pending {
                    Some(current) if current == *boundary => "folder is here now",
                    Some(current) if current < *boundary => "already undone",
                    _ => "enter undoes back to here",
                };
                OptionEntry {
                    label: crate::view::span::single_line(&prompt, 64),
                    description: format!("before this request · {state}"),
                    value: boundary.to_string(),
                }
            })
            .collect();
        self.open_overlay(Overlay::Options {
            title: "Timeline".into(),
            kind: OptionsKind::Timeline,
            entries,
        });
    }

    pub fn open_fork_overlay(&mut self) {
        let mut entries = vec![OptionEntry {
            label: "Fork current state".into(),
            description: "Copy the complete visible session".into(),
            value: "current".into(),
        }];
        entries.extend(self.items.iter().rev().filter_map(|item| {
            let haarness_protocol::ItemData::AssistantMessage { text, .. } = &item.data else {
                return None;
            };
            let label = text
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("Assistant reply");
            Some(OptionEntry {
                label: crate::view::span::single_line(label, 64),
                description: "Fork through this assistant reply".into(),
                value: item.id.to_string(),
            })
        }));
        self.open_overlay(Overlay::Options {
            title: "Fork session".into(),
            kind: OptionsKind::ForkMessages,
            entries,
        });
    }

    async fn fork_from_message(&mut self, value: &str, client: &ApiClient) -> Result<()> {
        let request = if value == "current" {
            haarness_protocol::ForkThreadRequest::default()
        } else {
            let id = value
                .parse::<uuid::Uuid>()
                .map(haarness_protocol::ItemId)
                .map_err(|_| anyhow::anyhow!("invalid fork message ID"))?;
            let sequence = self
                .items
                .iter()
                .find(|item| {
                    item.id == id
                        && matches!(
                            item.data,
                            haarness_protocol::ItemData::AssistantMessage { .. }
                        )
                })
                .map(|item| item.sequence)
                .ok_or_else(|| anyhow::anyhow!("fork message is no longer available"))?;
            haarness_protocol::ForkThreadRequest {
                title: None,
                up_to_sequence: Some(sequence),
                before_item_id: None,
            }
        };
        let thread = client.fork_thread_with(self.thread.id, request).await?;
        self.refresh_threads(client).await?;
        self.switch_thread(thread, client).await?;
        Ok(())
    }

    /// Undo repeatedly until the workspace is back at `value`. Each step is
    /// the same conflict-checked delta `/undo` applies, so a human edit in the
    /// way stops the walk with the reason rather than clobbering it.
    async fn undo_to_checkpoint(&mut self, value: &str, client: &ApiClient) -> Result<()> {
        let Ok(target) = value.parse::<u64>() else {
            return Ok(());
        };
        let mut steps = 0_usize;
        let mut stopped: Option<String> = None;
        loop {
            let reached = self
                .thread
                .revert
                .as_ref()
                .is_some_and(|state| state.sequence <= target);
            if reached {
                break;
            }
            match client.revert_to(self.thread.id, Some(target)).await {
                Ok(thread) => {
                    let progressed = thread.revert.as_ref().map(|state| state.sequence)
                        != self.thread.revert.as_ref().map(|state| state.sequence);
                    self.thread = thread;
                    steps += 1;
                    if !progressed {
                        break;
                    }
                }
                Err(error) => {
                    if steps == 0 {
                        self.notice = undo_error(&error, "Undo");
                        return Ok(());
                    }
                    stopped = Some(format!(
                        "Undid {steps} change{} · stopped: {error}",
                        plural(steps)
                    ));
                    break;
                }
            }
            // A runaway guard: the transcript cannot hold more checkpoints
            // than it holds items.
            if steps > self.items.len() {
                break;
            }
        }
        self.items = client.items(self.thread.id).await?;
        self.todos = client.todos(self.thread.id).await.unwrap_or_default();
        self.notice = stopped.unwrap_or_else(|| {
            self.undo_notice(&format!("Undone {steps} change{}", plural(steps)))
        });
        Ok(())
    }

    /// How many more undo and redo steps this thread has, counted from the
    /// paired checkpoints in the transcript. `/undo` and `/redo` say so, the
    /// way a text editor's history does, instead of leaving the reader to
    /// guess whether another press does anything.
    pub fn undo_depth(&self) -> (usize, usize) {
        let mut boundaries: Vec<u64> = self
            .items
            .iter()
            .filter_map(|item| match &item.data {
                haarness_protocol::ItemData::Snapshot {
                    boundary_sequence: Some(boundary),
                    base_snapshot_id: Some(_),
                    ..
                } => Some(*boundary),
                _ => None,
            })
            .collect();
        boundaries.sort_unstable();
        boundaries.dedup();
        match self.thread.revert.as_ref() {
            Some(state) => {
                let position = boundaries
                    .iter()
                    .position(|boundary| *boundary == state.sequence);
                match position {
                    Some(position) => (position, boundaries.len() - position),
                    // A legacy overlay revert can be redone but not extended.
                    None => (0, 1),
                }
            }
            None => (boundaries.len(), 0),
        }
    }

    /// The prompt of the earliest turn `/undo` took back, which the composer
    /// offers for editing until the next turn confirms the undo.
    pub(crate) fn undone_prompt(&self) -> Option<(String, Vec<String>)> {
        let boundary = self.thread.revert.as_ref()?.sequence;
        self.items
            .iter()
            .filter(|item| item.sequence > boundary)
            .find_map(|item| match &item.data {
                haarness_protocol::ItemData::UserMessage {
                    text,
                    attachments,
                    synthetic: false,
                    ..
                } => Some((text.clone(), attachments.clone())),
                _ => None,
            })
    }

    fn undo_notice(&self, verb: &str) -> String {
        let (undo, redo) = self.undo_depth();
        let mut parts = vec![verb.to_owned()];
        if undo > 0 {
            parts.push(format!("{undo} more undo step{}", plural(undo)));
        }
        if redo > 0 {
            parts.push(format!("/redo restores {redo} step{}", plural(redo)));
        }
        parts.join(" · ")
    }

    /// Reconcile the review viewer with the width it is about to be drawn at.
    /// Split needs a wide pane, so a resize can change the resolved layout
    /// and therefore which rows the pane holds.
    ///
    /// This is the review surface's one per-frame hook, so it is also where a
    /// finished background load is taken delivery of — before the frame that
    /// would otherwise still paint the loading state.
    pub fn sync_diff_layout(&mut self, width: usize) {
        self.poll_diff_load();
        let Overlay::Diff(viewer) = &mut self.overlay else {
            return;
        };
        let tree = if viewer.tree_showing() && !viewer.help && width >= 76 {
            crate::diffview::TREE_WIDTH.min(width / 3) + 1
        } else {
            0
        };
        let pane = width.saturating_sub(tree);
        if viewer.pane_width.get() == pane {
            return;
        }
        let before = viewer.layout_in_use();
        viewer.pane_width.set(pane);
        if viewer.layout_in_use() != before {
            let active = viewer.active_file;
            viewer.rebuild_display();
            viewer.focus_file(active);
        }
    }

    /// Open the full-screen review for `source`, remembering the layout the
    /// reader last chose.
    ///
    /// The viewer appears immediately, in its loading state, and the request
    /// runs on a worker task. Awaiting it here would hold the event loop for
    /// as long as the server takes, which on a large repository meant a TUI
    /// that could not repaint, could not stream, and could not be escaped.
    pub(crate) fn open_diff_viewer(
        &mut self,
        source: crate::diffview::DiffSource,
        client: &ApiClient,
    ) {
        let preferences = crate::config::load_diff_preferences();
        let context_step = preferences.context_step;
        let mut viewer = crate::diffview::DiffViewer::new(source, Vec::new(), preferences);
        viewer.loading = true;
        self.open_overlay(Overlay::Diff(Box::new(viewer)));
        self.start_diff_load(source, context_step, client);
    }

    /// Refetch the open review after its source or context changed, keeping
    /// the reader on the same file where that file still exists.
    // Kept async for its input-handler call site, which awaits every action.
    #[allow(clippy::unused_async)]
    pub(crate) async fn reload_diff_viewer(&mut self, client: &ApiClient) {
        let Overlay::Diff(viewer) = &self.overlay else {
            return;
        };
        self.start_diff_load(viewer.source, viewer.context_step, client);
    }

    /// Put a review request on a worker task, replacing whatever was already
    /// in flight: the reader has moved on, so its answer is already stale.
    fn start_diff_load(
        &mut self,
        source: crate::diffview::DiffSource,
        context_step: usize,
        client: &ApiClient,
    ) {
        if let Some(previous) = self.diff_load.take() {
            previous.cancel();
        }
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let client = client.clone();
        let thread = self.thread.id;
        let task = tokio::spawn(async move {
            let _ = sender.send(run_diff_load(client, thread, source, context_step).await);
        });
        self.diff_load = Some(DiffLoad { receiver, task });
    }

    /// Take delivery of a finished review load, if one is ready. Anything
    /// that no longer matches what the viewer is asking for — a closed
    /// overlay, a switched source, a changed context step — is dropped.
    pub(crate) fn poll_diff_load(&mut self) {
        let Some(load) = &mut self.diff_load else {
            return;
        };
        let completion = match load.receiver.try_recv() {
            Ok(completion) => completion,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                self.diff_load = None;
                return;
            }
        };
        self.diff_load = None;
        let Overlay::Diff(viewer) = &mut self.overlay else {
            return;
        };
        if viewer.source != completion.source || viewer.context_step != completion.context_step {
            return;
        }
        viewer.loading = false;
        match completion.result {
            Ok(review) => {
                viewer.omitted_files = review.omitted_files;
                viewer.set_files(review.files);
            }
            Err(error) => {
                // A first load that failed has nothing to show, so it leaves
                // the way it came. A failed refresh keeps the review the
                // reader already has.
                if viewer.files.is_empty() {
                    self.close_diff_viewer();
                }
                self.notice = error;
            }
        }
    }

    /// Leave the review, handing every queued comment to the composer so the
    /// notes travel with the next message instead of being lost.
    pub(crate) fn close_diff_viewer(&mut self) {
        // Escape means "stop": a load still in flight has nobody left to
        // deliver to, so cancel it rather than let it finish unwatched.
        if let Some(load) = self.diff_load.take() {
            load.cancel();
        }
        let Overlay::Diff(viewer) = &self.overlay else {
            self.close_overlay();
            return;
        };
        crate::config::save_diff_preferences(viewer.preferences());
        let comments = viewer.comments.clone();
        self.close_overlay();
        if comments.is_empty() {
            return;
        }
        let note = crate::diffview::format_comments(&comments);
        // As sent, so the draft's attached references come back as chips
        // rather than as their placeholders' bare text.
        let existing = self.composer.expanded_text();
        let combined = if existing.trim().is_empty() {
            note
        } else {
            format!("{existing}\n\n{note}")
        };
        self.composer.set_text(combined);
        self.notice = format!(
            "{} review comment{} added to your message",
            comments.len(),
            if comments.len() == 1 { "" } else { "s" }
        );
    }

    pub fn is_known_command(&self, raw: &str) -> bool {
        let name = raw
            .trim()
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or_default();
        COMMANDS.iter().any(|entry| {
            entry.name.eq_ignore_ascii_case(name)
                || entry
                    .aliases
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(name))
        }) || self
            .server_commands
            .iter()
            .any(|command| command.name.eq_ignore_ascii_case(name))
            || self
                .workflow_definitions
                .iter()
                .any(|workflow| workflow.invocation.eq_ignore_ascii_case(name))
    }

    /// The status-line answer to a command nothing answers to, naming the
    /// real one when the name reads as a typo of it.
    pub(crate) fn unknown_command_notice(&self, name: &str) -> String {
        match self.closest_command(name) {
            Some(suggestion) => self.language.format(
                "Unknown command /{name} · did you mean /{suggestion}?",
                &[("name", name), ("suggestion", &suggestion)],
            ),
            None => self.language.format(
                "Unknown command /{name} · start with a space to send it as text",
                &[("name", name)],
            ),
        }
    }

    fn closest_command(&self, name: &str) -> Option<String> {
        let name = name.to_ascii_lowercase();
        let limit = if name.chars().count() <= 4 { 1 } else { 2 };
        COMMANDS
            .iter()
            .flat_map(|entry| std::iter::once(entry.name).chain(entry.aliases.iter().copied()))
            .map(str::to_owned)
            .chain(
                self.server_commands
                    .iter()
                    .map(|command| command.name.clone()),
            )
            .chain(
                self.workflow_definitions
                    .iter()
                    .map(|workflow| workflow.invocation.clone()),
            )
            .map(|candidate| {
                (
                    edit_distance(&name, &candidate.to_ascii_lowercase()),
                    candidate,
                )
            })
            .filter(|(distance, _)| *distance <= limit)
            // On a tie, a slip that kept the length (`/stauts`) meant the
            // name of that length (`/status`, not `/stats`).
            .min_by_key(|(distance, candidate)| {
                (
                    *distance,
                    candidate.chars().count().abs_diff(name.chars().count()),
                )
            })
            .map(|(_, candidate)| candidate)
    }

    fn open_tool_catalog(&mut self, mut catalog: haarness_protocol::ToolCatalogResponse) {
        catalog
            .tools
            .sort_by(|left, right| left.group.cmp(&right.group).then(left.id.cmp(&right.id)));
        let entries = catalog
            .tools
            .into_iter()
            .map(|tool| {
                let state = if !tool.enabled {
                    "disabled"
                } else if tool.advertised {
                    "resident"
                } else {
                    "deferred"
                };
                let marker = match state {
                    "disabled" => "○",
                    "resident" => "●",
                    _ => "◐",
                };
                let action = if tool.enabled { "disable" } else { "enable" };
                OptionEntry {
                    label: format!("{marker} {}", tool.id),
                    description: format!(
                        "{} · {} · ~{} tokens · {}",
                        tool.group_title, state, tool.estimated_tokens, tool.description
                    ),
                    value: format!("{action}:{}", tool.id),
                }
            })
            .collect();
        self.open_overlay(Overlay::Options {
            title: format!(
                "Tools · lazy {}{} · ~{} / ~{} tokens (saved ~{})",
                catalog.lazy_mode.as_str(),
                if catalog.lazy_active { " active" } else { "" },
                catalog.estimated_advertised_tokens,
                catalog.estimated_total_tokens,
                catalog.estimated_saved_tokens,
            ),
            kind: OptionsKind::Tools,
            entries,
        });
    }

    async fn execute_tool_choice(&mut self, value: &str, client: &ApiClient) -> Result<()> {
        let Some((action, name)) = value.split_once(':') else {
            self.notice = "Invalid tool catalog action".into();
            return Ok(());
        };
        let enabled = match action {
            "enable" => true,
            "disable" => false,
            _ => {
                self.notice = "Invalid tool catalog action".into();
                return Ok(());
            }
        };
        client.update_tool(name, Some(enabled)).await?;
        let catalog = client.tool_catalog(Some(self.thread.id)).await?;
        self.open_tool_catalog(catalog);
        self.notice = format!(
            "{} tool {name} · setting persists for this folder",
            if enabled { "Enabled" } else { "Disabled" }
        );
        Ok(())
    }

    /// Install the authoritative workspace snapshot into a searchable modal.
    /// Rows carry stable workspace IDs; names and paths remain presentation,
    /// so duplicate names in different projects cannot target the wrong one.
    pub fn open_workspace_dashboard(&mut self, response: WorkspaceListResponse) {
        let current_project = response
            .current_workspace_id
            .as_deref()
            .and_then(|current| {
                response
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.id == current)
            })
            .and_then(|workspace| {
                response
                    .projects
                    .iter()
                    .find(|project| project.id == workspace.project_id)
            });
        let mut entries = vec![if current_project.is_some_and(|project| project.git) {
            OptionEntry {
                label: "＋ Create worktree".into(),
                description: "Create a managed Git worktree from this project".into(),
                value: "create".into(),
            }
        } else {
            OptionEntry {
                label: "Worktree creation unavailable".into(),
                description: "The current project is not a Git repository".into(),
                value: "create-unavailable".into(),
            }
        }];
        entries.extend(response.workspaces.iter().map(|workspace| {
            let project = response
                .projects
                .iter()
                .find(|project| project.id == workspace.project_id)
                .map_or("Unknown project", |project| project.name.as_str());
            let current = response.current_workspace_id.as_deref() == Some(workspace.id.as_str());
            let ownership = if workspace.managed {
                "managed worktree"
            } else {
                "local checkout"
            };
            let availability = if !workspace.exists {
                "missing"
            } else if current {
                "current"
            } else {
                "available"
            };
            OptionEntry {
                label: format!("{project} / {}", workspace.name),
                description: format!(
                    "{} · {ownership} · {availability} · {}",
                    workspace.branch.as_deref().unwrap_or("no branch"),
                    workspace.path
                ),
                value: format!("workspace:{}", workspace.id),
            }
        }));
        self.workspace_catalog = Some(response);
        self.workspace_selected = None;
        self.open_overlay(Overlay::Options {
            title: "Projects and worktrees".into(),
            kind: OptionsKind::Workspaces,
            entries,
        });
    }

    fn cached_workspace(&self) -> Option<WorkspaceInfo> {
        let selected = self.workspace_selected.as_deref()?;
        self.workspace_catalog
            .as_ref()?
            .workspaces
            .iter()
            .find(|workspace| workspace.id == selected)
            .cloned()
    }

    pub(crate) fn reopen_workspace_dashboard(&mut self) {
        if let Some(catalog) = self.workspace_catalog.clone() {
            self.open_workspace_dashboard(catalog);
        } else {
            self.close_overlay();
        }
    }

    pub(crate) fn open_workspace_create_name(&mut self, initial: Option<String>) {
        self.workspace_create_name = None;
        self.open_overlay(Overlay::Options {
            title: "Create worktree · name".into(),
            kind: OptionsKind::WorkspaceCreateName,
            entries: vec![OptionEntry {
                label: "Continue".into(),
                description: "Next: choose an optional base ref".into(),
                value: "continue".into(),
            }],
        });
        if let Some(initial) = initial {
            self.overlay_filter = initial;
        }
    }

    pub(crate) fn reopen_workspace_create_base_picker(&mut self, selected: Option<&str>) {
        let name = self.workspace_create_name.as_deref().unwrap_or_default();
        let mut entries = Vec::new();
        if let Some(refs) = &self.workspace_refs {
            let default_kind = if refs.refs.iter().any(|reference| reference.current) {
                "Current branch"
            } else {
                "Current commit"
            };
            entries.push(OptionEntry {
                label: format!("Default · {}", workspace_default_display(refs)),
                description: format!(
                    "{default_kind} · {}",
                    refs.default_commit.chars().take(12).collect::<String>()
                ),
                value: "default".into(),
            });
            for (kind, category, description) in [
                (WorkspaceRefKind::LocalBranch, "Local", "local branch"),
                (WorkspaceRefKind::RemoteBranch, "Remote", "remote branch"),
                (WorkspaceRefKind::Tag, "Tag", "tag"),
            ] {
                entries.extend(
                    refs.refs
                        .iter()
                        .filter(|reference| reference.kind == kind && !reference.current)
                        .map(|reference| OptionEntry {
                            label: format!("{category} · {}", reference.name),
                            description: format!(
                                "{description} · {}",
                                reference.commit.chars().take(12).collect::<String>()
                            ),
                            value: format!("ref:{}", reference.reference),
                        }),
                );
            }
        } else {
            entries.push(OptionEntry {
                label: "Default · primary checkout".into(),
                description: "Use its current branch or detached commit".into(),
                value: "default".into(),
            });
        }
        entries.push(OptionEntry {
            label: "Custom · Type a branch, tag, or commit…".into(),
            description: "Validate an arbitrary Git ref before creating".into(),
            value: "custom".into(),
        });
        self.open_overlay(Overlay::Options {
            title: format!("Create “{name}” · base ref"),
            kind: OptionsKind::WorkspaceCreateBase,
            entries,
        });
        if let Some(selected) = selected
            && let Overlay::Options { ref entries, .. } = self.overlay
        {
            self.selection = entries
                .iter()
                .position(|entry| entry.value == selected)
                .unwrap_or(0);
        }
    }

    fn open_workspace_create_custom_base(&mut self) {
        let name = self.workspace_create_name.as_deref().unwrap_or_default();
        self.open_overlay(Overlay::Options {
            title: format!("Create “{name}” · custom ref"),
            kind: OptionsKind::WorkspaceCreateCustomBase,
            entries: vec![OptionEntry {
                label: "Check and create".into(),
                description: "Validate this branch, tag, or commit on the server".into(),
                value: "check".into(),
            }],
        });
        self.overlay_filter.clone_from(&self.workspace_custom_ref);
    }

    fn open_workspace_actions(&mut self, workspace_id: &str) {
        self.workspace_selected = Some(workspace_id.to_owned());
        let Some(workspace) = self.cached_workspace() else {
            self.notice = "This folder is no longer in the current catalog".into();
            self.reopen_workspace_dashboard();
            return;
        };
        let current = self
            .workspace_catalog
            .as_ref()
            .and_then(|catalog| catalog.current_workspace_id.as_deref())
            == Some(workspace.id.as_str());
        let mut state = vec![if workspace.managed {
            "managed worktree"
        } else {
            "local checkout"
        }];
        if current {
            state.push("current");
        }
        if !workspace.exists {
            state.push("missing");
        }
        let mut entries = vec![OptionEntry {
            label: workspace.name.clone(),
            description: format!(
                "{} · {} · {}",
                workspace.branch.as_deref().unwrap_or("no branch"),
                state.join(" · "),
                workspace.path
            ),
            value: "info".into(),
        }];
        if workspace.exists && !current && self.workspace_local_switching {
            entries.push(OptionEntry {
                label: "Open folder".into(),
                description: "Open this project folder or worktree".into(),
                value: "open".into(),
            });
        }
        if workspace.managed && workspace.exists && !current {
            entries.push(OptionEntry {
                label: "Reset worktree…".into(),
                description: "Review reset choices before changing Git state".into(),
                value: "reset".into(),
            });
        }
        if workspace.managed && !current {
            entries.push(OptionEntry {
                label: if workspace.exists {
                    "Remove worktree…".into()
                } else {
                    "Remove missing folder…".into()
                },
                description: "Review exactly what will be removed before confirming".into(),
                value: "remove".into(),
            });
        }
        entries.push(OptionEntry {
            label: "Back to projects and worktrees".into(),
            description: "Return without making changes".into(),
            value: "back".into(),
        });
        self.open_overlay(Overlay::Options {
            title: format!("Folder · {}", workspace.name),
            kind: OptionsKind::WorkspaceActions,
            entries,
        });
        // Opening an available non-current workspace is the primary safe
        // action; current/missing entries remain on their informational row.
        if workspace.exists && !current && self.workspace_local_switching {
            self.selection = 1;
        }
    }

    pub(crate) fn reopen_workspace_actions(&mut self) {
        if let Some(workspace_id) = self.workspace_selected.clone() {
            self.open_workspace_actions(&workspace_id);
        } else {
            self.reopen_workspace_dashboard();
        }
    }

    fn open_workspace_reset_confirmation(&mut self) {
        let Some(workspace) = self.cached_workspace() else {
            self.reopen_workspace_dashboard();
            return;
        };
        let target = workspace.from_ref.as_deref().unwrap_or("HEAD");
        self.open_overlay(Overlay::Options {
            title: format!("Reset “{}” to {target}", workspace.name),
            kind: OptionsKind::WorkspaceResetConfirmation,
            entries: vec![
                OptionEntry {
                    label: "Cancel".into(),
                    description: "Keep the worktree exactly as it is".into(),
                    value: "cancel".into(),
                },
                OptionEntry {
                    label: "Reset clean worktree".into(),
                    description: format!(
                        "Move to {target}; commits after that base are discarded from this branch"
                    ),
                    value: "reset".into(),
                },
                OptionEntry {
                    label: "Reset and discard tracked changes".into(),
                    description:
                        "Discard commits after base and tracked edits; untracked files stay".into(),
                    value: "reset-discard".into(),
                },
                OptionEntry {
                    label: "Reset and remove untracked files".into(),
                    description:
                        "Discard commits and tracked edits; delete untracked (not ignored) files"
                            .into(),
                    value: "reset-clean".into(),
                },
            ],
        });
        self.selection = 0;
    }

    fn open_workspace_remove_confirmation(&mut self) {
        let Some(workspace) = self.cached_workspace() else {
            self.reopen_workspace_dashboard();
            return;
        };
        let missing = !workspace.exists;
        let entries = if missing {
            vec![
                OptionEntry {
                    label: "Cancel".into(),
                    description: "Keep the stale folder registration".into(),
                    value: "cancel".into(),
                },
                OptionEntry {
                    label: "Clean up stale registration".into(),
                    description:
                        "Remove only the stale Haarness registration; keep every Git branch and worktree metadata"
                            .into(),
                    value: "remove-stale".into(),
                },
            ]
        } else {
            vec![
                OptionEntry {
                    label: "Cancel".into(),
                    description: "Keep the folder and its registry entry".into(),
                    value: "cancel".into(),
                },
                OptionEntry {
                    label: "Remove clean worktree".into(),
                    description: "Delete the checkout; refuse if local changes exist".into(),
                    value: "remove".into(),
                },
                OptionEntry {
                    label: "Remove and discard local changes".into(),
                    description: "Force-remove the checkout; keep its Git branch".into(),
                    value: "remove-force".into(),
                },
                OptionEntry {
                    label: "Remove and delete merged branch".into(),
                    description: "Remove a clean checkout, then safely delete its merged branch"
                        .into(),
                    value: "remove-branch".into(),
                },
                OptionEntry {
                    label: "Force remove and delete merged branch".into(),
                    description:
                        "Discard local changes; branch deletion still refuses unmerged work".into(),
                    value: "remove-force-branch".into(),
                },
            ]
        };
        self.open_overlay(Overlay::Options {
            title: format!("Remove “{}”", workspace.name),
            kind: OptionsKind::WorkspaceRemoveConfirmation,
            entries,
        });
        self.selection = 0;
    }

    fn show_workspace_error(&mut self, action: &str, message: String) {
        self.notice = format!("Folder {action} failed · {message}");
        self.show_toast(
            Some(format!("Folder {action} failed")),
            message,
            ToastVariant::Error,
            std::time::Duration::from_secs(8),
        );
    }

    fn start_workspace_ref_operation(&mut self, operation: WorkspaceRefOperation) -> AppAction {
        if self.workspace_mutation.is_some() || self.workspace_ref_operation.is_some() {
            self.notice = "A folder operation is already running".into();
            return AppAction::Continue;
        }
        let detail = match &operation {
            WorkspaceRefOperation::Load => "Discovering local branches, remote branches, and tags",
            WorkspaceRefOperation::Resolve { reference } => {
                self.workspace_custom_ref.clone_from(reference);
                "Resolving the typed ref against the current Git repository"
            }
        };
        self.workspace_ref_operation = Some(operation.clone());
        self.open_overlay(Overlay::Panel {
            title: operation.progress_title().into(),
            lines: vec![
                detail.into(),
                "The request is running · live events and rendering remain active".into(),
                "Duplicate and cancel input is disabled until it finishes".into(),
            ],
        });
        self.notice = operation.progress_title().trim_end_matches('…').into();
        AppAction::StartWorkspaceRefOperation(operation)
    }

    fn start_workspace_creation(
        &mut self,
        from_ref: Option<String>,
        retry: WorkspaceCreateRetry,
    ) -> AppAction {
        let Some(name) = self.workspace_create_name.clone() else {
            self.open_workspace_create_name(None);
            return AppAction::Continue;
        };
        self.start_workspace_mutation(WorkspaceMutation::Create {
            request: WorkspaceCreateRequest {
                name,
                from_ref,
                branch: None,
                path: None,
            },
            retry,
        })
    }

    /// Apply an asynchronous ref read and transition to the next create step.
    /// A successful custom resolution immediately schedules the separately
    /// nonblocking create mutation; errors restore the exact typed surface.
    pub(crate) fn finish_workspace_ref_operation(
        &mut self,
        completion: WorkspaceRefOperationCompletion,
    ) -> AppAction {
        self.workspace_ref_operation = None;
        let WorkspaceRefOperationCompletion { operation, result } = completion;
        match (operation, result) {
            (WorkspaceRefOperation::Load, Ok(WorkspaceRefOperationResult::Loaded(references))) => {
                self.workspace_refs = Some(references);
                self.reopen_workspace_create_base_picker(None);
                self.notice = "Choose a worktree base ref".into();
                AppAction::Continue
            }
            (WorkspaceRefOperation::Load, Err(message)) => {
                tracing::warn!(error = %message, "workspace ref discovery failed");
                self.workspace_refs = None;
                self.reopen_workspace_create_base_picker(None);
                self.notice =
                    "Could not load base refs · Default and Custom are still available".into();
                self.show_toast(
                    Some("Could not load base refs".into()),
                    "Choose Default or type a Custom branch, tag, or commit".into(),
                    ToastVariant::Error,
                    std::time::Duration::from_secs(8),
                );
                AppAction::Continue
            }
            (
                WorkspaceRefOperation::Resolve { reference },
                Ok(WorkspaceRefOperationResult::Resolved(resolved)),
            ) => self.start_workspace_creation(
                Some(resolved.reference),
                WorkspaceCreateRetry::Custom { input: reference },
            ),
            (WorkspaceRefOperation::Resolve { reference }, Err(message)) => {
                tracing::warn!(error = %message, %reference, "workspace ref validation failed");
                self.workspace_custom_ref.clone_from(&reference);
                self.open_workspace_create_custom_base();
                let user_message = format!("Unknown branch, tag, or commit: “{reference}”");
                self.notice.clone_from(&user_message);
                self.show_toast(
                    Some("Base ref was not found".into()),
                    user_message,
                    ToastVariant::Error,
                    std::time::Duration::from_secs(8),
                );
                AppAction::Continue
            }
            // A mismatched completion is an internal programming error, not a
            // reason to discard the user's in-progress create flow.
            (operation, Ok(_)) => {
                tracing::warn!(
                    operation = operation.progress_title(),
                    "workspace ref completion kind mismatch"
                );
                self.reopen_workspace_create_base_picker(None);
                AppAction::Continue
            }
        }
    }

    fn start_workspace_mutation(&mut self, operation: WorkspaceMutation) -> AppAction {
        if self.workspace_mutation.is_some() || self.workspace_ref_operation.is_some() {
            self.notice = "A folder operation is already running".into();
            return AppAction::Continue;
        }
        let detail = match &operation {
            WorkspaceMutation::Open { .. } => self.cached_workspace().map_or_else(
                || "Opening the selected folder".into(),
                |workspace| format!("Opening “{}”", workspace.name),
            ),
            WorkspaceMutation::Create { request, .. } => format!(
                "Creating “{}” from {}",
                request.name,
                request.from_ref.as_deref().unwrap_or_else(|| {
                    self.workspace_refs
                        .as_ref()
                        .map_or("current branch", workspace_default_display)
                })
            ),
            WorkspaceMutation::Reset { .. } => self.cached_workspace().map_or_else(
                || "Resetting the selected worktree".into(),
                |workspace| format!("Resetting “{}”", workspace.name),
            ),
            WorkspaceMutation::Remove { .. } => self.cached_workspace().map_or_else(
                || "Removing the selected worktree".into(),
                |workspace| format!("Removing “{}”", workspace.name),
            ),
            WorkspaceMutation::Cleanup { .. } => self.cached_workspace().map_or_else(
                || "Cleaning up the stale folder registration".into(),
                |workspace| format!("Cleaning up stale registration “{}”", workspace.name),
            ),
        };
        self.workspace_mutation = Some(operation.clone());
        self.open_overlay(Overlay::Panel {
            title: operation.progress_title().into(),
            lines: vec![
                detail,
                "Git operation is running · live events and rendering remain active".into(),
                "This operation cannot be safely cancelled or started twice".into(),
            ],
        });
        self.notice = operation.progress_title().trim_end_matches('…').into();
        AppAction::StartWorkspaceMutation(operation)
    }

    /// Apply a background mutation result and restore the exact retry surface
    /// on failure. Returns a local path only when the launcher can safely
    /// detach and relaunch this TUI in the target workspace.
    pub(crate) fn finish_workspace_mutation(
        &mut self,
        completion: WorkspaceMutationCompletion,
        local_switching: bool,
    ) -> Option<std::path::PathBuf> {
        self.workspace_mutation = None;
        let WorkspaceMutationCompletion {
            operation,
            result,
            catalog,
        } = completion;
        let response = match result {
            Ok(response) => response,
            Err(message) => {
                let action = operation.verb();
                match &operation {
                    WorkspaceMutation::Open { .. } => self.reopen_workspace_actions(),
                    WorkspaceMutation::Create { request, retry } => {
                        self.workspace_create_name = Some(request.name.clone());
                        match retry {
                            WorkspaceCreateRetry::Picker { value } => {
                                self.reopen_workspace_create_base_picker(Some(value));
                            }
                            WorkspaceCreateRetry::Custom { input } => {
                                self.workspace_custom_ref.clone_from(input);
                                self.open_workspace_create_custom_base();
                            }
                        }
                    }
                    WorkspaceMutation::Reset { selection, .. } => {
                        self.open_workspace_reset_confirmation();
                        self.selection = *selection;
                    }
                    WorkspaceMutation::Remove { selection, .. }
                    | WorkspaceMutation::Cleanup { selection, .. } => {
                        self.open_workspace_remove_confirmation();
                        self.selection = *selection;
                    }
                }
                self.show_workspace_error(action, message);
                return None;
            }
        };

        if local_switching
            && matches!(
                &operation,
                WorkspaceMutation::Open { .. } | WorkspaceMutation::Create { .. }
            )
        {
            return Some(std::path::PathBuf::from(response.workspace.path));
        }

        if let Some(catalog) = catalog {
            self.open_workspace_dashboard(catalog);
        } else {
            // A catalog refresh is helpful but not required for correctness.
            // Reconcile the successful structured response into the cached
            // snapshot so the modal never lies after a transient GET failure.
            if let Some(catalog) = &mut self.workspace_catalog {
                match &operation {
                    WorkspaceMutation::Create { .. } => {
                        catalog.workspaces.push(response.workspace.clone());
                    }
                    WorkspaceMutation::Reset { .. } | WorkspaceMutation::Open { .. } => {
                        if let Some(workspace) = catalog
                            .workspaces
                            .iter_mut()
                            .find(|workspace| workspace.id == response.workspace.id)
                        {
                            *workspace = response.workspace.clone();
                        }
                    }
                    WorkspaceMutation::Remove { .. } | WorkspaceMutation::Cleanup { .. } => catalog
                        .workspaces
                        .retain(|workspace| workspace.id != response.workspace.id),
                }
            }
            if matches!(&operation, WorkspaceMutation::Open { .. }) {
                self.reopen_workspace_actions();
            } else {
                self.reopen_workspace_dashboard();
            }
        }

        self.workspace_create_name = None;
        self.workspace_refs = None;
        self.workspace_custom_ref.clear();
        self.notice = match operation {
            WorkspaceMutation::Create { .. } => format!(
                "Created worktree “{}” from {}{} · folder switching is unavailable on attached clients",
                response.workspace.name,
                response
                    .workspace
                    .from_ref
                    .as_deref()
                    .unwrap_or("current branch"),
                response
                    .workspace
                    .branch
                    .as_deref()
                    .map_or_else(String::new, |branch| format!(" on branch {branch}")),
            ),
            WorkspaceMutation::Open { .. } => format!(
                "Folder “{}” opened · folder switching is unavailable on attached clients",
                response.workspace.name
            ),
            WorkspaceMutation::Reset { .. } => format!(
                "Reset worktree “{}” to {}{}",
                response.workspace.name,
                response.workspace.from_ref.as_deref().unwrap_or("its base"),
                response.workspace.base_commit.as_deref().map_or_else(
                    String::new,
                    |commit| format!(" ({})", commit.chars().take(12).collect::<String>()),
                )
            ),
            WorkspaceMutation::Remove { request, .. } => {
                let branch = if request.delete_branch {
                    response.workspace.branch.as_deref().map_or_else(
                        || " · deleted merged branch".to_owned(),
                        |branch| format!(" · retained branch {branch}"),
                    )
                } else {
                    response
                        .workspace
                        .branch
                        .as_deref()
                        .map_or_else(String::new, |branch| format!(" · kept branch {branch}"))
                };
                format!("Removed worktree “{}”{branch}", response.workspace.name)
            }
            WorkspaceMutation::Cleanup { .. } => format!(
                "Cleaned up stale folder registration “{}” · kept every Git branch",
                response.workspace.name
            ),
        };
        None
    }

    fn execute_workspace_dashboard_choice(&mut self, value: &str) -> AppAction {
        if value == "create" {
            self.workspace_refs = None;
            self.workspace_custom_ref.clear();
            self.open_workspace_create_name(None);
        } else if value == "create-unavailable" {
            self.reopen_workspace_dashboard();
            self.notice = "Worktree creation requires a Git project".into();
        } else if let Some(workspace_id) = value.strip_prefix("workspace:") {
            self.open_workspace_actions(workspace_id);
        }
        AppAction::Continue
    }

    fn execute_workspace_action(&mut self, value: &str) -> AppAction {
        match value {
            "back" => self.reopen_workspace_dashboard(),
            "reset" => self.open_workspace_reset_confirmation(),
            "remove" => self.open_workspace_remove_confirmation(),
            "open" => {
                if !self.workspace_local_switching {
                    self.notice = "Folder switching is unavailable on attached clients".into();
                    self.reopen_workspace_actions();
                    return AppAction::Continue;
                }
                let Some(workspace) = self.cached_workspace() else {
                    self.reopen_workspace_dashboard();
                    return AppAction::Continue;
                };
                return self.start_workspace_mutation(WorkspaceMutation::Open {
                    reference: workspace.id,
                });
            }
            "info" => self.reopen_workspace_actions(),
            _ => {}
        }
        AppAction::Continue
    }

    fn execute_workspace_create_name(&mut self, value: &str) -> AppAction {
        let name = value.trim();
        if name.is_empty() {
            self.notice = "Worktree name is required".into();
            self.open_workspace_create_name(None);
        } else {
            self.workspace_create_name = Some(name.to_owned());
            self.workspace_refs = None;
            return self.start_workspace_ref_operation(WorkspaceRefOperation::Load);
        }
        AppAction::Continue
    }

    fn execute_workspace_create_base(&mut self, value: &str) -> AppAction {
        match value {
            "default" => self.start_workspace_creation(
                None,
                WorkspaceCreateRetry::Picker {
                    value: "default".into(),
                },
            ),
            "custom" => {
                self.open_workspace_create_custom_base();
                AppAction::Continue
            }
            _ => value
                .strip_prefix("ref:")
                .map_or(AppAction::Continue, |reference| {
                    self.start_workspace_creation(
                        Some(reference.to_owned()),
                        WorkspaceCreateRetry::Picker {
                            value: value.to_owned(),
                        },
                    )
                }),
        }
    }

    fn execute_workspace_create_custom_base(&mut self, value: &str) -> AppAction {
        value.clone_into(&mut self.workspace_custom_ref);
        let reference = value.trim();
        if reference.is_empty() {
            self.notice = "A branch, tag, or commit is required".into();
            self.open_workspace_create_custom_base();
            return AppAction::Continue;
        }
        self.start_workspace_ref_operation(WorkspaceRefOperation::Resolve {
            reference: reference.to_owned(),
        })
    }

    fn execute_workspace_reset_confirmation(&mut self, value: &str) -> AppAction {
        if value == "cancel" {
            self.reopen_workspace_actions();
            return AppAction::Continue;
        }
        let Some(workspace) = self.cached_workspace() else {
            self.reopen_workspace_dashboard();
            return AppAction::Continue;
        };
        let (request, selection) = match value {
            "reset" => (
                WorkspaceResetRequest {
                    confirm: true,
                    ..WorkspaceResetRequest::default()
                },
                1,
            ),
            "reset-discard" => (
                WorkspaceResetRequest {
                    confirm: true,
                    discard_changes: true,
                    ..WorkspaceResetRequest::default()
                },
                2,
            ),
            "reset-clean" => (
                WorkspaceResetRequest {
                    confirm: true,
                    discard_changes: true,
                    remove_untracked: true,
                    to_ref: None,
                },
                3,
            ),
            _ => return AppAction::Continue,
        };
        self.start_workspace_mutation(WorkspaceMutation::Reset {
            reference: workspace.id,
            request,
            selection,
        })
    }

    fn execute_workspace_remove_confirmation(&mut self, value: &str) -> AppAction {
        if value == "cancel" {
            self.reopen_workspace_actions();
            return AppAction::Continue;
        }
        let Some(workspace) = self.cached_workspace() else {
            self.reopen_workspace_dashboard();
            return AppAction::Continue;
        };
        if !workspace.exists {
            if value != "remove-stale" {
                self.open_workspace_remove_confirmation();
                self.notice = "Missing folder cleanup always keeps its Git branch".into();
                return AppAction::Continue;
            }
            return self.start_workspace_mutation(WorkspaceMutation::Cleanup {
                reference: workspace.id,
                request: WorkspaceCleanupRequest { confirm: true },
                selection: 1,
            });
        }
        let (request, selection) = match value {
            "remove" => (
                WorkspaceRemoveRequest {
                    confirm: true,
                    ..WorkspaceRemoveRequest::default()
                },
                1,
            ),
            "remove-force" => (
                WorkspaceRemoveRequest {
                    confirm: true,
                    discard_changes: true,
                    delete_branch: false,
                },
                2,
            ),
            "remove-branch" => (
                WorkspaceRemoveRequest {
                    confirm: true,
                    discard_changes: false,
                    delete_branch: true,
                },
                3,
            ),
            "remove-force-branch" => (
                WorkspaceRemoveRequest {
                    confirm: true,
                    discard_changes: true,
                    delete_branch: true,
                },
                4,
            ),
            _ => return AppAction::Continue,
        };
        self.start_workspace_mutation(WorkspaceMutation::Remove {
            reference: workspace.id,
            request,
            selection,
        })
    }

    pub async fn execute_choice(
        &mut self,
        choice: OverlayChoice,
        client: &ApiClient,
    ) -> Result<AppAction> {
        match choice {
            OverlayChoice::Command(command) => self.execute_command(&command, client).await,
            OverlayChoice::Thread(thread) => {
                self.resume_session(*thread, client).await?;
                Ok(AppAction::Continue)
            }
            OverlayChoice::Model(model) => {
                self.set_model(&model, client).await?;
                Ok(AppAction::Continue)
            }
            OverlayChoice::Agent(agent) => {
                self.set_agent(&agent, client).await?;
                Ok(AppAction::Continue)
            }
            OverlayChoice::Option(OptionsKind::InputQueue, value) => {
                self.input_queue_actions(&value)?;
                Ok(AppAction::Continue)
            }
            OverlayChoice::Option(OptionsKind::InputQueueActions, value) => {
                self.input_queue_action(&value, client).await?;
                Ok(AppAction::Continue)
            }
            OverlayChoice::Option(OptionsKind::Tasks, value) => {
                if let Ok(child_id) = value.parse::<uuid::Uuid>() {
                    self.enter_child(haarness_protocol::ThreadId(child_id), client)
                        .await?;
                }
                Ok(AppAction::Continue)
            }
            OverlayChoice::Option(OptionsKind::Timeline, value) => {
                self.undo_to_checkpoint(&value, client).await?;
                Ok(AppAction::Continue)
            }
            OverlayChoice::Option(OptionsKind::ForkMessages, value) => {
                self.fork_from_message(&value, client).await?;
                Ok(AppAction::Continue)
            }
            OverlayChoice::Option(OptionsKind::Messages, value) => {
                self.open_message_actions(&value)?;
                Ok(AppAction::Continue)
            }
            OverlayChoice::Option(OptionsKind::MessageActions, value) => {
                self.execute_message_action(&value, client).await?;
                Ok(AppAction::Continue)
            }
            OverlayChoice::Option(OptionsKind::Workspaces, value) => {
                Ok(self.execute_workspace_dashboard_choice(&value))
            }
            OverlayChoice::Option(OptionsKind::WorkspaceActions, value) => {
                Ok(self.execute_workspace_action(&value))
            }
            OverlayChoice::Option(OptionsKind::WorkspaceCreateName, value) => {
                Ok(self.execute_workspace_create_name(&value))
            }
            OverlayChoice::Option(OptionsKind::WorkspaceCreateBase, value) => {
                Ok(self.execute_workspace_create_base(&value))
            }
            OverlayChoice::Option(OptionsKind::WorkspaceCreateCustomBase, value) => {
                Ok(self.execute_workspace_create_custom_base(&value))
            }
            OverlayChoice::Option(OptionsKind::WorkspaceResetConfirmation, value) => {
                Ok(self.execute_workspace_reset_confirmation(&value))
            }
            OverlayChoice::Option(OptionsKind::WorkspaceRemoveConfirmation, value) => {
                Ok(self.execute_workspace_remove_confirmation(&value))
            }
            OverlayChoice::Option(OptionsKind::SessionActions, value) => {
                self.execute_session_action(&value, client).await?;
                Ok(AppAction::Continue)
            }
            OverlayChoice::Option(
                kind @ (OptionsKind::Management
                | OptionsKind::ManagementField
                | OptionsKind::ManagementValue
                | OptionsKind::ManagementConfirm),
                value,
            ) => {
                self.management_choice(kind, &value, client).await?;
                Ok(AppAction::Continue)
            }
            OverlayChoice::Option(OptionsKind::ConnectProvider, value) => {
                self.execute_connect_choice(&value, client).await?;
                Ok(AppAction::Continue)
            }
            OverlayChoice::Option(OptionsKind::ConnectMethod, value) => {
                self.choose_connect_method(&value, client).await?;
                Ok(AppAction::Continue)
            }
            OverlayChoice::Option(OptionsKind::ConnectApiKey, value) => {
                self.submit_connect_key(&value, client).await?;
                Ok(AppAction::Continue)
            }
            OverlayChoice::Option(OptionsKind::Billing, value) => {
                if value == "signin" {
                    self.open_connect_picker(Some("haarness"), client).await?;
                } else {
                    self.close_overlay();
                    self.open_billing_page(&value);
                }
                Ok(AppAction::Continue)
            }
            OverlayChoice::Option(OptionsKind::CloudRouter, value) => {
                let account = haarness_account::Client::new(haarness_account::Config::from_env()?)?;
                let router = account.select_router(&value).await?;
                self.providers = client.refresh_providers().await?;
                self.notice = format!(
                    "Cloud router: {} · {}",
                    router["name"].as_str().unwrap_or(""),
                    if router["credentialMode"] == "byok" {
                        "Your keys; no Cloud credits"
                    } else {
                        "Cloud credits"
                    }
                );
                Ok(AppAction::Continue)
            }
            OverlayChoice::Option(OptionsKind::ReasoningEffort, value) => {
                self.set_reasoning_option(&value, client).await?;
                Ok(AppAction::Continue)
            }
            OverlayChoice::Option(OptionsKind::Language, value) => {
                self.apply_option(OptionsKind::Language, &value);
                Ok(AppAction::Continue)
            }
            OverlayChoice::Option(OptionsKind::Tools, value) => {
                self.execute_tool_choice(&value, client).await?;
                Ok(AppAction::Continue)
            }
            OverlayChoice::Option(OptionsKind::WorkflowDashboard, value) => {
                self.execute_workflow_dashboard_choice(&value, client)
                    .await?;
                Ok(AppAction::Continue)
            }
            OverlayChoice::Option(OptionsKind::WorkflowRunActions, value) => {
                self.execute_workflow_run_action(&value, client).await?;
                Ok(AppAction::Continue)
            }
            OverlayChoice::Option(OptionsKind::GoalActions, value) => {
                self.execute_goal_choice(&value, client).await?;
                Ok(AppAction::Continue)
            }
            OverlayChoice::Option(OptionsKind::GoalClearConfirmation, value) => {
                self.execute_goal_clear_confirmation(&value, client).await?;
                Ok(AppAction::Continue)
            }
            OverlayChoice::Option(OptionsKind::GoalCompletion, value) => {
                self.execute_goal_completion_choice(&value, client).await?;
                Ok(AppAction::Continue)
            }
            OverlayChoice::Theme(name, variant) => {
                if self.apply_theme(&name, &variant) {
                    self.theme_selected = true;
                    crate::config::save_theme_selection(&name, &variant);
                    self.notice = format!("Theme: {name} · {variant}");
                } else {
                    self.notice = format!("Unknown theme {name}");
                }
                Ok(AppAction::Continue)
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    pub async fn execute_command(&mut self, raw: &str, client: &ApiClient) -> Result<AppAction> {
        let command = raw.trim().trim_start_matches('/');
        let (name, arguments) = command
            .split_once(char::is_whitespace)
            .map_or((command, ""), |(name, arguments)| (name, arguments.trim()));
        let Some(spec) = COMMANDS.iter().find(|entry| {
            entry.name.eq_ignore_ascii_case(name)
                || entry
                    .aliases
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(name))
        }) else {
            // The advertised server registry is authoritative for configured
            // prompts and dynamic workflow invocations. The server applies
            // collision precedence and dispatches the winning command.
            if let Some(command) = self
                .server_commands
                .iter()
                .find(|candidate| candidate.name.eq_ignore_ascii_case(name))
                .cloned()
            {
                self.submit_server_command(&command, arguments, client)
                    .await?;
                return Ok(AppAction::Continue);
            }
            // Compatibility fallback for an older server which exposes the
            // catalog but has no shared command endpoint yet.
            if self
                .workflow_definitions
                .iter()
                .any(|workflow| workflow.invocation.eq_ignore_ascii_case(name))
            {
                let args = parse_workflow_args(arguments).map_err(anyhow::Error::msg)?;
                self.start_named_workflow(name.to_owned(), args, client)
                    .await?;
                return Ok(AppAction::Continue);
            }
            self.notice = self.unknown_command_notice(name);
            return Ok(AppAction::Continue);
        };
        match spec.name {
            "input-mode" => {
                let result = client
                    .manage(
                        "input-settings",
                        &serde_json::json!({"action":"command","arguments":arguments}),
                    )
                    .await?;
                self.notice = result["data"]["message"]
                    .as_str()
                    .unwrap_or("Input settings saved")
                    .into();
            }

            "permissions-mode" => match arguments.trim() {
                "auto" => self.change_approval_mode(true, client).await,
                "review" | "normal" => self.change_approval_mode(false, client).await,
                "" => {
                    self.change_approval_mode(!self.auto_permissions, client)
                        .await
                }
                _ => self.notice = "Usage: /permissions-mode [review|auto]".into(),
            },
            "new" if self.is_untouched_session() => {
                self.notice = "This session is already new · type to start".into();
            }
            "new" => {
                let request = CreateThreadRequest {
                    id: None,
                    schedule: None,
                    title: None,
                    parent_id: None,
                    agent: Some(self.agent().into()),
                    provider: Some(self.thread.provider.clone()),
                    model: Some(self.thread.model.clone()),
                    ultracode: None,
                };
                let thread = client.create_thread(request).await?;
                // A new chat starts in the open chat's mode, as it would
                // have when Auto belonged to the TUI rather than the chat.
                if self.approval_server && self.auto_permissions {
                    let auto = haarness_protocol::SetApprovalModeRequest {
                        mode: haarness_protocol::ApprovalModeSetting::Auto,
                        revision: None,
                    };
                    if let Err(error) = client.set_approval_mode(thread.id, &auto).await {
                        tracing::warn!(%error, "could not start the new chat in Auto");
                    }
                }
                self.refresh_threads(client).await?;
                self.switch_thread(thread, client).await?;
            }
            "sessions" => {
                self.open_sessions(arguments, client).await?;
            }
            "worktree" => {
                let (action, tail) = arguments
                    .split_once(char::is_whitespace)
                    .map_or((arguments, ""), |(action, tail)| (action, tail.trim()));
                match action.to_ascii_lowercase().as_str() {
                    "" | "list" | "ls" => {
                        let response = client.workspaces().await?;
                        self.workspace_local_switching = client.supports_local_workspace_switch();
                        self.open_workspace_dashboard(response);
                    }
                    "open" | "switch" => {
                        if tail.is_empty() {
                            self.notice = "Usage: /worktree open <name-or-id>".into();
                        } else if !client.supports_local_workspace_switch() {
                            self.notice =
                                "Folder switching is unavailable on attached clients".into();
                        } else {
                            return Ok(self.start_workspace_mutation(WorkspaceMutation::Open {
                                reference: tail.to_owned(),
                            }));
                        }
                    }
                    "create" => match parse_workspace_create(tail) {
                        Ok(request) => {
                            self.workspace_create_name = Some(request.name.clone());
                            let retry = request.from_ref.clone().map_or_else(
                                || WorkspaceCreateRetry::Picker {
                                    value: "default".into(),
                                },
                                |input| WorkspaceCreateRetry::Custom { input },
                            );
                            return Ok(self.start_workspace_mutation(WorkspaceMutation::Create {
                                request,
                                retry,
                            }));
                        }
                        Err(error) => self.notice = error,
                    },
                    "reset" => match parse_workspace_reset(tail) {
                        Ok((reference, request)) => {
                            let selection = if request.remove_untracked {
                                3
                            } else if request.discard_changes {
                                2
                            } else {
                                1
                            };
                            return Ok(self.start_workspace_mutation(WorkspaceMutation::Reset {
                                reference,
                                request,
                                selection,
                            }));
                        }
                        Err(error) => self.notice = error,
                    },
                    "remove" | "rm" => match parse_workspace_remove(tail) {
                        Ok((reference, request)) => {
                            let selection = match (request.discard_changes, request.delete_branch) {
                                (false, false) => 1,
                                (true, false) => 2,
                                (false, true) => 3,
                                (true, true) => 4,
                            };
                            return Ok(self.start_workspace_mutation(WorkspaceMutation::Remove {
                                reference,
                                request,
                                selection,
                            }));
                        }
                        Err(error) => self.notice = error,
                    },
                    _ => {
                        self.notice = "Use /worktree [list|open|create|reset|remove]".into();
                    }
                }
            }
            "queue" => {
                if arguments.is_empty() {
                    self.open_input_queue(client).await?;
                } else {
                    self.defer_prompt(arguments.to_owned(), client).await?;
                }
            }
            "models" => {
                if arguments.is_empty() {
                    match client.refresh_providers().await {
                        Ok(providers) => self.providers = providers,
                        Err(_) => {
                            self.notice =
                                "Could not refresh models; showing the saved catalog".into()
                        }
                    }
                    if self.any_provider_connected() {
                        self.open_overlay(Overlay::Models);
                    } else {
                        // Nothing to pick from yet: connecting is the model
                        // choice, so go straight there.
                        self.connect.onboarding = true;
                        self.open_connect_picker(None, client).await?;
                    }
                } else {
                    self.set_model(arguments, client).await?;
                }
            }
            "connect" => {
                self.connect.onboarding = !self.any_provider_connected();
                self.open_connect_picker(crate::connect::connect_target(arguments), client)
                    .await?;
            }
            "providers" | "mcp" | "schedules" | "hooks" | "memory" | "profiles" | "stash"
            | "keybinds" | "servers" | "usage-statistics" => {
                self.open_management(spec.name, &serde_json::json!({"action":"view"}), client)
                    .await?;
            }
            "share" => {
                self.open_management("sharing", &serde_json::json!({"action":"view"}), client)
                    .await?;
            }
            "marketplace" => {
                // The same server-authored pages as Desktop's Settings ›
                // Marketplace: search, inspect, install, then installed resources.
                let input = if arguments == "installed" {
                    serde_json::json!({"action":"installed"})
                } else {
                    // The first page, searching for the text when there is one.
                    serde_json::json!({"action":"view","q":arguments})
                };
                self.open_management("marketplace-ui", &input, client)
                    .await?;
            }
            "remote-control" => self.remote_control_command(arguments, client)?,
            "chrome" => self.chrome_command(arguments, client)?,
            "usage" => {
                let usage = client.thread_usage(self.thread.id).await?;
                let number = |key: &str| usage[key].as_u64().unwrap_or(0);
                let cost = usage["costUsd"]
                    .as_f64()
                    .map_or_else(|| "Unavailable".into(), |v| format!("${v:.6}"));
                let mut lines = vec![
                    format!("Session: {}", self.thread.title),
                    format!("Model: {}/{}", self.thread.provider, self.thread.model),
                    format!("Input tokens: {}", number("inputTokens")),
                    format!("Output tokens: {}", number("outputTokens")),
                    format!("Cache read tokens: {}", number("cacheReadTokens")),
                    format!("Cache write tokens: {}", number("cacheWriteTokens")),
                    format!("Cost: {cost}"),
                ];
                if let Some(window) = self
                    .providers
                    .iter()
                    .find(|p| p.id == self.thread.provider)
                    .and_then(|p| p.models.iter().find(|m| m.id == self.thread.model))
                    .and_then(|m| m.context_window)
                {
                    lines.push(format!("Context window: {window} tokens"));
                }
                if usage["costIncomplete"] == true {
                    lines.push("Some turns have no price; shown cost is a known subtotal.".into());
                }
                if let Some(latest) = usage["latestInputTokens"].as_u64() {
                    lines.push(format!(
                        "Last request input: {latest} tokens (includes provider context)"
                    ));
                }
                if let Some(pace) = self.request_pace_label() {
                    lines.push(
                        self.language
                            .format("Last request: {details}", &[("details", &pace)]),
                    );
                }
                lines.push(format!("Conversation items: {}", self.items.len()));
                if let Some(descendants) = usage.get("descendants") {
                    lines.push(format!("Descendant sessions: {}", descendants["count"]));
                    lines.push(format!(
                        "Descendant input/output tokens: {} / {}",
                        descendants["inputTokens"], descendants["outputTokens"]
                    ));
                    lines.push(format!(
                        "Descendant known cost: {}",
                        descendants["costUsd"]
                            .as_f64()
                            .map_or_else(|| "Unavailable".into(), |v| format!("${v:.6}"))
                    ));
                }
                lines.push("Reasoning tokens are not reported separately by this server. Reopen /usage to refresh.".into());
                self.open_overlay(Overlay::Panel {
                    title: "Session usage and context".into(),
                    lines,
                });
            }
            "agents" => {
                if arguments.is_empty() {
                    self.open_overlay(Overlay::Agents);
                } else {
                    self.set_agent(arguments, client).await?;
                }
            }
            "plan" | "review" | "build" | "explore" => {
                self.set_agent(spec.name, client).await?;
                if !arguments.is_empty() {
                    let _ = self.submit_prompt(arguments.into(), client).await?;
                }
            }
            "compact" => {
                client
                    .compact(
                        self.thread.id,
                        CompactRequest {
                            // A bare command asks the server to generate a real
                            // model summary. Explicit text remains available for
                            // advanced/manual checkpointing.
                            summary: (!arguments.is_empty()).then(|| arguments.into()),
                            files_touched: Vec::new(),
                            outstanding: Vec::new(),
                            todo_checkpoint: None,
                            tail_start_sequence: None,
                        },
                    )
                    .await?;
                self.items = client.items(self.thread.id).await?;
                self.notice = "Compaction checkpoint created".into();
            }
            "diff" => match crate::diffview::DiffSource::parse(&arguments.to_ascii_lowercase()) {
                Some(source) => self.open_diff_viewer(source, client),
                None => self.notice = "Usage: /diff [git|branch|last]".into(),
            },
            "terminal" => {
                let create = match arguments.to_ascii_lowercase().as_str() {
                    "" => false,
                    "new" => true,
                    _ => {
                        self.notice = "Usage: /terminal [new]".into();
                        return Ok(AppAction::Continue);
                    }
                };
                // A shell that exited is a closed tab: forget it rather than
                // reattach to its last screen.
                let (mut terminals, exited): (Vec<_>, Vec<_>) = client
                    .ptys()
                    .await?
                    .into_iter()
                    .partition(|info| info.status != haarness_protocol::PtyStatus::Exited);
                for info in exited {
                    let _ = client.delete_pty(info.id).await;
                }
                let created = if create || terminals.is_empty() {
                    // `/terminal new` always opens one; a first terminal is
                    // shared with a window that opens one at the same time.
                    let info = client
                        .create_pty(&PtyCreateRequest {
                            if_none: !create,
                            ..PtyCreateRequest::default()
                        })
                        .await?;
                    terminals.push(info.clone());
                    Some(info.id)
                } else {
                    None
                };
                for info in &terminals {
                    client.connect_pty(info.id, 0);
                }
                let mut workspace = crate::pty_view::TerminalWorkspace::new(terminals);
                if let Some(id) = created {
                    workspace.focus(id);
                }
                self.terminal = Some(workspace);
                self.notice = "Terminal attached · Ctrl+] returns to chat".into();
            }
            "undo" => {
                if self.active_turn.is_some() {
                    self.notice = "Stopping and undoing…".into();
                }
                match client.revert(self.thread.id).await {
                    Ok(thread) => {
                        self.thread = thread;
                        let restored = self.undone_prompt();
                        self.items = client.items(self.thread.id).await?;
                        self.todos = client.todos(self.thread.id).await.unwrap_or_default();
                        self.undo_draft = None;
                        if let Some((text, attachments)) = restored {
                            self.composer.set_text(text);
                            self.attachments = attachments;
                        }
                        self.notice = self.undo_notice("Undone — the next turn confirms");
                    }
                    Err(error) => self.notice = undo_error(&error, "Undo"),
                }
            }
            "timeline" => self.open_timeline_overlay(),
            "messages" => self.open_messages((!arguments.is_empty()).then_some(arguments)),
            "redo" => {
                let held = self.holds_undo_draft();
                match client.unrevert(self.thread.id).await {
                    Ok(thread) => {
                        // The undone prompt is back in the transcript; a copy
                        // left in the composer would only send it twice.
                        self.undo_draft = None;
                        if held {
                            self.composer.clear();
                            self.attachments.clear();
                        }
                        self.thread = thread;
                        self.items = client.items(self.thread.id).await?;
                        self.todos = client.todos(self.thread.id).await.unwrap_or_default();
                        self.notice = self.undo_notice("Redone");
                    }
                    Err(error) => self.notice = undo_error(&error, "Redo"),
                }
            }
            "fork" => {
                if arguments.is_empty() {
                    self.open_fork_overlay();
                } else {
                    self.notice = "Use bare /fork and choose a message boundary".into();
                }
            }
            "rename" => {
                if arguments.is_empty() {
                    self.notice = "Usage: /rename [session-id] <title>".into();
                } else if self.matching_session_id(arguments).is_some() {
                    self.notice = "Type a title after the session ID".into();
                } else {
                    let (target, title) = self.rename_target(arguments);
                    let updated = client
                        .update_thread(
                            target,
                            UpdateThreadRequest {
                                title: Some(title.into()),
                                archived: None,
                                agent: None,
                                provider: None,
                                model: None,
                                ultracode: None,
                            },
                        )
                        .await?;
                    if target == self.thread.id {
                        self.thread = updated.clone();
                    }
                    self.upsert_thread(updated);
                    self.notice = "Session renamed".into();
                }
            }
            "archive" => {
                self.archive_current_session(client).await?;
            }
            "reasoning" => match arguments {
                "" => self.open_overlay(Overlay::Options {
                    title: "Reasoning".into(),
                    kind: OptionsKind::ReasoningEffort,
                    entries: reasoning_entries(self),
                }),
                effort => {
                    self.set_reasoning_option(effort, client).await?;
                }
            },
            "ultracode" => {
                let enabled = match arguments {
                    "" | "toggle" => !self.thread.ultracode,
                    "on" | "enable" => true,
                    "off" | "disable" => false,
                    _ => {
                        self.notice = "Usage: /ultracode [on|off]".into();
                        return Ok(AppAction::Continue);
                    }
                };
                self.set_ultracode(enabled, client).await?;
            }
            "workflow" => {
                self.execute_workflow_command(arguments, client).await?;
            }
            "btw" => {
                if arguments.is_empty() {
                    self.notice = "Usage: /btw <side question>".into();
                } else {
                    let response = client.btw(self.thread.id, arguments.to_owned()).await?;
                    self.open_overlay(Overlay::Panel {
                        title: "BTW · isolated read-only answer".into(),
                        lines: response.answer.lines().map(str::to_owned).collect(),
                    });
                    self.notice = format!(
                        "BTW answer · transcript unchanged · {} input / {} output tokens",
                        response.input_tokens, response.output_tokens
                    );
                }
            }
            "themes" => {
                if arguments.is_empty() {
                    self.open_overlay(Overlay::Themes);
                } else {
                    let (name, variant) = arguments
                        .split_once(char::is_whitespace)
                        .map_or((arguments, "dark"), |(name, variant)| {
                            (name, variant.trim())
                        });
                    if self.apply_theme(name, variant) {
                        self.theme_selected = true;
                        crate::config::save_theme_selection(name, variant);
                        self.notice = format!("Theme: {name} · {variant}");
                    } else {
                        self.notice = format!("Unknown theme {name} ({variant})");
                    }
                }
            }
            "language" => {
                if arguments.is_empty() {
                    self.open_overlay(Overlay::Options {
                        title: self.language.text("select_language").into(),
                        kind: OptionsKind::Language,
                        entries: language_entries(self),
                    });
                } else if let Some(language) = crate::locale::Language::parse(arguments) {
                    self.language = language;
                    crate::config::save_language(language);
                    self.notice = language_notice(language);
                } else {
                    self.notice = self
                        .language
                        .text("Choose a language with /language.")
                        .into();
                }
            }
            "plugin" => {
                let mut words = arguments.split_whitespace();
                let action = words.next().unwrap_or_default();
                if action.is_empty() || action == "list" || action == "search" {
                    let query = if action == "search" {
                        words.collect::<Vec<_>>().join(" ").to_ascii_lowercase()
                    } else {
                        String::new()
                    };
                    let plugins = client.plugins().await?;
                    let lines = plugins
                        .into_iter()
                        .filter(|plugin| {
                            query.is_empty()
                                || plugin.name.to_ascii_lowercase().contains(&query)
                                || plugin.description.to_ascii_lowercase().contains(&query)
                        })
                        .map(|plugin| {
                            format!(
                                "{}  {}  {}  {}",
                                if plugin.enabled { "●" } else { "○" },
                                plugin.name,
                                plugin.source,
                                plugin.description
                            )
                        })
                        .collect::<Vec<_>>();
                    self.open_overlay(Overlay::Panel {
                        title: "Plugins".into(),
                        lines: if lines.is_empty() {
                            vec![
                                "No matching plugins".into(),
                                "Use /plugin install <name@marketplace|local-path> to add a Haarness plugin".into(),
                            ]
                        } else {
                            lines
                        },
                    });
                } else {
                    let (action, target) = if action == "marketplace" {
                        let subcommand = words.next().unwrap_or_default();
                        if subcommand != "add" {
                            self.notice = "Usage: /plugin marketplace add <path-or-url>".into();
                            return Ok(AppAction::Continue);
                        }
                        ("marketplace", words.collect::<Vec<_>>().join(" "))
                    } else {
                        (action, words.collect::<Vec<_>>().join(" "))
                    };
                    if target.is_empty()
                        || !matches!(
                            action,
                            "install"
                                | "update"
                                | "enable"
                                | "disable"
                                | "remove"
                                | "uninstall"
                                | "marketplace"
                        )
                    {
                        self.notice = "Use /plugin, /plugin search <text>, /plugin install <name@marketplace|local-path>, /plugin update|enable|disable|remove <name>, or /plugin marketplace add <path-or-url>".into();
                    } else {
                        client
                            .plugin_action(haarness_protocol::PluginActionRequest {
                                action: action.into(),
                                target,
                            })
                            .await?;
                        self.notice = match action {
                            "install" => "Plugin installed · reload queued until active work is idle",
                            "enable" => "Plugin enabled · reload queued until active work is idle",
                            "disable" => "Plugin disabled · reload queued until active work is idle",
                            "remove" | "uninstall" => {
                                "Plugin removed · reload queued until active work is idle"
                            }
                            "marketplace" => "Marketplace added · install its plugins with /plugin install <name>@<marketplace>",
                            _ => "Plugin updated · reload queued until active work is idle",
                        }
                        .into();
                    }
                }
            }
            "skills" => {
                let skills = client.skills().await?;
                self.open_overlay(Overlay::Panel {
                    title: "Skills".into(),
                    lines: if skills.is_empty() {
                        vec![
                            "No skills discovered".into(),
                            "Add one in .haarness/skills/<name>/SKILL.md".into(),
                        ]
                    } else {
                        skills
                            .into_iter()
                            .map(|skill| format!("{}  {}", skill.name, skill.description))
                            .collect()
                    },
                });
            }
            "tools" => {
                let words = arguments.split_whitespace().collect::<Vec<_>>();
                match words.as_slice() {
                    [] | ["list"] => {
                        let catalog = client.tool_catalog(Some(self.thread.id)).await?;
                        self.open_tool_catalog(catalog);
                    }
                    ["show" | "on"] => {
                        self.tool_details = true;
                        self.notice =
                            "Tool details shown — /tools hide returns to compact activity".into();
                    }
                    ["hide" | "off"] => {
                        self.tool_details = false;
                        self.notice =
                            "Tool details compact — click a row or /tools show to inspect".into();
                    }
                    ["lazy", "reset"] => {
                        client.update_lazy_tools(None).await?;
                        let catalog = client.tool_catalog(Some(self.thread.id)).await?;
                        let effective = catalog.lazy_mode;
                        self.open_tool_catalog(catalog);
                        self.notice = format!(
                            "Lazy tool loading reset to configured/default mode: {}",
                            effective.as_str()
                        );
                    }
                    ["lazy", mode] => match mode.parse::<haarness_protocol::LazyToolMode>() {
                        Ok(mode) => {
                            client.update_lazy_tools(Some(mode)).await?;
                            // The mutation response is workspace-global. Fetch
                            // the active thread's view so provider context and
                            // session-local promoted schemas remain accurate.
                            let catalog = client.tool_catalog(Some(self.thread.id)).await?;
                            self.open_tool_catalog(catalog);
                            self.notice = format!("Lazy tool loading: {}", mode.as_str());
                        }
                        Err(error) => self.notice = error,
                    },
                    [action @ ("enable" | "disable" | "reset"), name] => {
                        let enabled = match *action {
                            "enable" => Some(true),
                            "disable" => Some(false),
                            _ => None,
                        };
                        client.update_tool(name, enabled).await?;
                        self.notice = match enabled {
                            Some(true) => format!("Enabled tool {name}"),
                            Some(false) => format!("Disabled tool {name}"),
                            None => format!("Reset tool {name} to configured/default availability"),
                        };
                    }
                    _ => {
                        self.notice = "Usage: /tools [list|show|hide|lazy <auto|always|off|reset>|enable <tool>|disable <tool>|reset <tool>]".into();
                    }
                }
            }
            "router" => {
                let account = haarness_account::Client::new(haarness_account::Config::from_env()?)?;
                let catalog = match account.routers().await {
                    Ok(catalog) => catalog,
                    // Signed out: sign in right here, as /billing offers, rather
                    // than sending the person to a shell command.
                    Err(haarness_account::Error::NotLoggedIn) => {
                        self.open_connect_picker(Some("haarness"), client).await?;
                        self.notice = "Sign in to Haarness Cloud to choose a router".into();
                        return Ok(AppAction::Continue);
                    }
                    Err(error) => return Err(error.into()),
                };
                let selected = account.config.selected_router()?;
                let entries = catalog["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|router| OptionEntry {
                        label: format!(
                            "{}{}",
                            if selected.as_deref() == router["id"].as_str()
                                || (selected.is_none() && router["isDefault"] == true)
                            {
                                "✓ "
                            } else {
                                ""
                            },
                            router["name"].as_str().unwrap_or("Router")
                        ),
                        description: if router["credentialMode"] == "byok" {
                            "Your provider keys · no Cloud credits".into()
                        } else {
                            "System provider keys · uses Cloud credits".into()
                        },
                        value: router["id"].as_str().unwrap_or_default().into(),
                    })
                    .collect();
                self.open_overlay(Overlay::Options {
                    title: "Cloud router on this computer".into(),
                    kind: OptionsKind::CloudRouter,
                    entries,
                });
            }
            "billing" => {
                if arguments.is_empty() {
                    self.open_billing();
                } else if matches!(arguments, "open" | "topup" | "usage") {
                    self.open_billing_page(arguments);
                } else {
                    self.notice = "Usage: /billing [open|topup|usage]".into();
                }
            }
            "status" => {
                let lsp = client.lsp_status().await.unwrap_or_default();
                let formatters = client.formatter_status().await.unwrap_or_default();
                let chrome = crate::chrome::fetch_hosts(client, self.thread.id).await;
                let mut lines = vec![
                    format!("thread   {}", self.thread.id),
                    format!("agent    {}", self.agent()),
                    format!("model    {}/{}", self.thread.provider, self.thread.model),
                    format!(
                        "mode     {}",
                        if self.thread.ultracode {
                            "ultracode"
                        } else {
                            "standard"
                        }
                    ),
                    format!("items    {}", self.items.len()),
                    format!(
                        "turn     {}",
                        if self.active_turn.is_some() {
                            "running"
                        } else {
                            "idle"
                        }
                    ),
                    format!(
                        "Chrome   {}",
                        crate::chrome::status_line(&chrome, self.language)
                    ),
                ];
                lines.push(format!("LSP      {} configured", lsp.len()));
                lines.extend(lsp.into_iter().map(|server| {
                    let detail = server
                        .error
                        .map_or_else(String::new, |error| format!(" · {error}"));
                    format!("  {}  {}{detail}", server.id, server.status)
                }));
                lines.push(format!("format   {} configured", formatters.len()));
                lines.extend(
                    formatters
                        .into_iter()
                        .map(|formatter| format!("  {}  {}", formatter.id, formatter.status)),
                );
                self.open_overlay(Overlay::Panel {
                    title: "Status".into(),
                    lines,
                });
            }
            "stats" | "insights" => {
                let result = async {
                    let (query, open) = analytics_query(arguments, spec.name == "insights")?;
                    let document = client.insights(&query).await?;
                    let summary = document["summary"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("missing insights summary"))?;
                    // The dialog already carries the report's title.
                    let mut lines: Vec<String> = summary
                        .lines()
                        .skip_while(|line| line.starts_with("Haarness · "))
                        .map(str::to_owned)
                        .collect();
                    if spec.name == "insights" {
                        use std::io::Write as _;
                        let html = document["html"]
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("missing insights HTML"))?;
                        let path = std::env::temp_dir()
                            .join(format!("haarness-insights-{}.html", uuid::Uuid::now_v7()));
                        let mut options = std::fs::OpenOptions::new();
                        options.write(true).create_new(true);
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::OpenOptionsExt as _;
                            options.mode(0o600);
                        }
                        options.open(&path)?.write_all(html.as_bytes())?;
                        crate::terminal::copy_to_clipboard(&path.display().to_string());
                        // A browser opened over SSH would open on the far
                        // machine, if anywhere.
                        let state = if !open {
                            "path copied · /insights --open opens it"
                        } else if crate::connect::over_ssh() {
                            "path copied · not opened over SSH"
                        } else if crate::terminal::open_file(&path) {
                            "opened in your browser"
                        } else {
                            "path copied · no browser to open it"
                        };
                        lines.insert(0, format!("Report: {} ({state})", path.display()));
                    }
                    Ok::<_, anyhow::Error>(lines)
                }
                .await;
                match result {
                    Ok(lines) => self.open_overlay(Overlay::Panel {
                        title: "Cross-project insights".into(),
                        lines,
                    }),
                    Err(error) => self.notice = format!("Insights failed: {error}"),
                }
            }
            "goal" => {
                let control = arguments.to_ascii_lowercase();
                let management = match control.as_str() {
                    "status" => Some(GoalActionRequest::Status),
                    "pause" => Some(GoalActionRequest::Pause),
                    "resume" => Some(GoalActionRequest::Resume),
                    "complete" => Some(GoalActionRequest::Complete),
                    "block" => Some(GoalActionRequest::Block { reason: None }),
                    _ => None,
                };
                if arguments.is_empty() {
                    let goal = client.goal(self.thread.id).await?;
                    self.install_goal_snapshot(goal);
                    self.open_goal_overlay();
                } else if control == "edit" {
                    let goal = client.goal(self.thread.id).await?;
                    self.install_goal_snapshot(goal);
                    self.prefill_goal(self.goal.as_ref().map(|goal| goal.goal.clone()));
                } else if control == "clear" {
                    let goal = client.goal(self.thread.id).await?;
                    self.install_goal_snapshot(goal);
                    if self.goal.is_some() {
                        self.open_goal_clear_confirmation();
                    } else {
                        self.open_goal_overlay();
                    }
                } else if let Some(action) = management {
                    let response = client.goal_action(self.thread.id, &action).await?;
                    self.install_goal_response(response);
                    if matches!(action, GoalActionRequest::Complete) {
                        self.reveal_goal_completion();
                    } else if !matches!(action, GoalActionRequest::Clear) {
                        self.open_goal_overlay();
                    }
                } else if arguments.chars().count() > 4_000 {
                    self.notice = "Goal is too long (maximum 4,000 characters)".into();
                } else {
                    let objective = arguments.trim().to_owned();
                    let response = client
                        .goal_action(
                            self.thread.id,
                            &GoalActionRequest::Set {
                                goal: objective.clone(),
                                token_budget: None,
                            },
                        )
                        .await?;
                    self.install_goal_response(response);
                    let started = if self.active_turn.is_some() {
                        self.queue_prompt(objective, client).await?
                    } else {
                        self.submit_prompt(objective, client).await?
                    };
                    if !started {
                        self.notice = "Goal saved; connect a provider to start pursuing it".into();
                    }
                }
            }
            "loop" => {
                if arguments.is_empty() {
                    self.notice = "Usage: /loop <interval, e.g. 5m> <prompt> · /loop stop".into();
                } else if arguments == "stop" {
                    if client.get_loop(self.thread.id).await?.is_null() {
                        self.notice = "No loop is running".into();
                    } else {
                        client
                            .set_loop(self.thread.id, serde_json::json!({"stop": true}))
                            .await?;
                        self.notice = "Loop stopped".into();
                    }
                } else {
                    let (interval, prompt) = arguments
                        .split_once(char::is_whitespace)
                        .map_or((arguments, ""), |(interval, prompt)| {
                            (interval, prompt.trim())
                        });
                    match (parse_interval(interval), prompt.is_empty()) {
                        (Some(seconds), false) => {
                            match client
                                .set_loop(
                                    self.thread.id,
                                    serde_json::json!({
                                        "prompt": prompt,
                                        "intervalSeconds": seconds,
                                    }),
                                )
                                .await
                            {
                                Ok(_) => {
                                    self.notice = format!(
                                        "Looping every {interval}: runs after each turn                                          completes (7-day expiry) · /loop stop ends it"
                                    );
                                }
                                Err(error) => self.notice = format!("Loop failed: {error}"),
                            }
                        }
                        _ => {
                            self.notice = "Usage: /loop <interval, e.g. 30s|5m|1h> <prompt>".into();
                        }
                    }
                }
            }
            "copy" => {
                let text = self.items.iter().rev().find_map(|item| match &item.data {
                    haarness_protocol::ItemData::AssistantMessage { text } => Some(text.clone()),
                    _ => None,
                });
                match text {
                    Some(text) => {
                        crate::terminal::copy_to_clipboard(&text);
                        self.notice = "Copied the last reply".into();
                    }
                    None => self.notice = "Nothing to copy yet".into(),
                }
            }
            "export" => {
                let path = std::env::temp_dir().join(format!(
                    "haarness-{}.md",
                    crate::view::span::short_id(&self.thread.id.to_string())
                ));
                match std::fs::write(&path, self.transcript_markdown()) {
                    Ok(()) => {
                        crate::terminal::copy_to_clipboard(&path.display().to_string());
                        self.notice = format!("Exported to {} (path copied)", path.display());
                    }
                    Err(error) => self.notice = format!("Export failed: {error}"),
                }
            }
            "editor" => return Ok(AppAction::OpenEditor),
            "tasks" => self.open_tasks_overlay(),
            "background" => {
                let child = if arguments.is_empty() {
                    None
                } else {
                    match arguments.parse::<uuid::Uuid>() {
                        Ok(child) => Some(haarness_protocol::ThreadId(child)),
                        Err(_) => {
                            self.notice = "Usage: /background [running-child-thread-id]".into();
                            return Ok(AppAction::Continue);
                        }
                    }
                };
                let promoted = client
                    .promote_background_tasks(self.thread.id, child)
                    .await?;
                self.notice = match promoted.len() {
                    0 => "No matching foreground subagent is running".into(),
                    1 => "Subagent moved to background · completion will return here".into(),
                    count => format!(
                        "{count} subagents moved to background · completions will return here"
                    ),
                };
            }
            "cancel" => {
                if self.active_turn.is_some() {
                    // `/stop` is the typed form of Esc Esc: same tree-wide
                    // stop, same answer for steering the agent had not read.
                    client.interrupt_session(self.thread.id, true).await?;
                    self.notice = "Interrupted".into();
                } else {
                    self.notice = "No active turn".into();
                }
            }
            "help" => self.open_overlay(Overlay::Commands),
            "exit" => return Ok(AppAction::Quit),
            _ => unreachable!(),
        }
        Ok(AppAction::Continue)
    }

    /// Apply a choice made in a generic options modal.
    pub fn apply_option(&mut self, kind: OptionsKind, value: &str) {
        match kind {
            // Handled async in execute_choice; nothing to do here.
            OptionsKind::CloudRouter
            | OptionsKind::Billing
            | OptionsKind::InputQueue
            | OptionsKind::InputQueueActions
            | OptionsKind::Management
            | OptionsKind::ManagementField
            | OptionsKind::ManagementValue
            | OptionsKind::ManagementConfirm
            | OptionsKind::ConnectProvider
            | OptionsKind::ConnectMethod
            | OptionsKind::ConnectApiKey
            | OptionsKind::Tasks
            | OptionsKind::Tools
            | OptionsKind::Timeline
            | OptionsKind::ForkMessages
            | OptionsKind::Messages
            | OptionsKind::MessageActions
            | OptionsKind::Workspaces
            | OptionsKind::WorkspaceActions
            | OptionsKind::WorkspaceCreateName
            | OptionsKind::WorkspaceCreateBase
            | OptionsKind::WorkspaceCreateCustomBase
            | OptionsKind::WorkspaceResetConfirmation
            | OptionsKind::WorkspaceRemoveConfirmation
            | OptionsKind::SessionActions
            | OptionsKind::WorkflowDashboard
            | OptionsKind::WorkflowRunActions
            | OptionsKind::GoalActions
            | OptionsKind::GoalClearConfirmation
            | OptionsKind::GoalCompletion => {}
            OptionsKind::Language => {
                if let Some(language) = crate::locale::Language::parse(value) {
                    self.language = language;
                    crate::config::save_language(language);
                    self.notice = language_notice(language);
                }
            }
            OptionsKind::ReasoningEffort => match value {
                "display" => {
                    self.reasoning = !self.reasoning;
                    self.notice = format!(
                        "Reasoning display {}",
                        if self.reasoning { "enabled" } else { "hidden" }
                    );
                }
                "off" => {
                    self.thread.ultracode = false;
                    self.reasoning_effort = None;
                    self.notice = "Reasoning effort: provider default".into();
                }
                "ultracode" => {
                    self.thread.ultracode = true;
                    self.reasoning_effort = None;
                    self.notice = "Ultracode enabled · strongest compatible reasoning".into();
                }
                effort => {
                    if !self
                        .reasoning_efforts()
                        .iter()
                        .any(|supported| supported == effort)
                    {
                        self.notice = self.unsupported_effort(effort);
                        return;
                    }
                    self.thread.ultracode = false;
                    self.reasoning_effort = Some(effort.to_owned());
                    self.notice = format!("Reasoning effort: {effort}");
                }
            },
        }
    }

    fn install_goal_response(&mut self, response: GoalActionResponse) {
        self.notice = response.message;
        self.install_goal_snapshot(response.state);
    }

    pub(crate) fn install_goal_snapshot(&mut self, goal: Option<GoalState>) {
        self.goal = goal.filter(|state| state.status != GoalStatus::Cleared);
        self.goal_snapshots
            .insert(self.thread.id, self.goal.clone());
        self.goal_refresh_needed = false;
        if self
            .goal
            .as_ref()
            .is_some_and(|goal| goal.status == GoalStatus::Complete)
        {
            self.goal_completion = Some(default_goal_completion());
        } else {
            self.goal_completion = None;
        }
        if self.goal.is_none() {
            self.last_goal_completion = None;
        }
    }

    pub fn open_goal_overlay(&mut self) {
        let mut entries = Vec::new();
        // The objective and why it stopped are printed in full above the
        // choices (see `goal_preface`); the row itself stays short.
        entries.push(OptionEntry {
            label: self.goal.as_ref().map_or_else(
                || "No active goal".into(),
                |goal| {
                    let label = goal_status_label(goal);
                    let state = label.split(" · ").next().unwrap_or_default();
                    format!("Goal {state}")
                },
            ),
            description: self
                .goal
                .as_ref()
                .map_or_else(|| "Set one with /goal <objective>".into(), goal_tokens),
            value: "current".into(),
        });
        entries.push(OptionEntry {
            label: if self.goal.is_some() {
                "Update goal".into()
            } else {
                "Start goal".into()
            },
            description: "Set the objective and immediately start an assistant turn".into(),
            value: "edit".into(),
        });
        entries.push(OptionEntry {
            label: "Refresh status".into(),
            description: "Read the latest durable goal state from the server".into(),
            value: "status".into(),
        });
        if self
            .goal
            .as_ref()
            .is_some_and(|goal| goal.status == GoalStatus::Active)
        {
            entries.push(OptionEntry {
                label: "Pause".into(),
                description: "Stop automatic continuations without clearing progress".into(),
                value: "pause".into(),
            });
        } else if self
            .goal
            .as_ref()
            .is_some_and(|goal| goal.status == GoalStatus::Paused)
        {
            entries.push(OptionEntry {
                label: "Resume".into(),
                description: "Reactivate pursuit; the next message continues it".into(),
                value: "resume".into(),
            });
        }
        if self.goal.is_some() {
            entries.push(OptionEntry {
                label: "Clear".into(),
                description: "Remove the standing goal and its tracked state".into(),
                value: "clear".into(),
            });
        }
        self.open_overlay(Overlay::Options {
            title: "Goal".into(),
            kind: OptionsKind::GoalActions,
            entries,
        });
    }

    fn open_goal_clear_confirmation(&mut self) {
        let objective = self.goal.as_ref().map_or_else(
            || "No active goal".into(),
            |goal| format!("Clear “{}”?", single_line_goal(&goal.goal, 72)),
        );
        self.open_overlay(Overlay::Options {
            title: "Clear goal".into(),
            kind: OptionsKind::GoalClearConfirmation,
            entries: vec![
                OptionEntry {
                    label: "Keep goal".into(),
                    description: "Cancel and return to goal controls".into(),
                    value: "cancel".into(),
                },
                OptionEntry {
                    label: "Clear goal".into(),
                    description: format!("{objective} Remove its tracked state."),
                    value: "confirm".into(),
                },
            ],
        });
        // The non-destructive choice must remain the Enter-key default.
        self.selection = 0;
    }

    pub fn open_goal_completion(&mut self, completion: haarness_protocol::GoalCompletion) {
        let entries = [
            (&completion.edit, "edit"),
            (&completion.new, "new"),
            (&completion.clear, "clear"),
            (&completion.dismiss, "dismiss"),
        ]
        .into_iter()
        .map(|(option, value)| OptionEntry {
            label: option.label.clone(),
            description: option.description.clone(),
            value: value.into(),
        })
        .collect();
        let title = completion.title.clone();
        self.goal_completion = Some(completion);
        self.open_overlay(Overlay::Options {
            title,
            kind: OptionsKind::GoalCompletion,
            entries,
        });
        // Dismiss is the safest default: Enter cannot silently start or clear
        // another pursuit when the dialog first appears.
        self.selection = 3;
    }

    pub fn reveal_goal_completion(&mut self) {
        let Some(goal) = self.goal.as_ref() else {
            return;
        };
        if goal.status != GoalStatus::Complete
            || self.last_goal_completion.as_deref() == Some(&goal.goal_id)
            || self.active_turn.is_some()
            || !matches!(self.overlay, Overlay::None)
            || !self.pending_questions.is_empty()
            || self.permission_focus
        {
            return;
        }
        let Some(completion) = self.goal_completion.clone() else {
            return;
        };
        self.last_goal_completion = Some(goal.goal_id.clone());
        self.open_goal_completion(completion);
    }

    async fn execute_goal_choice(&mut self, value: &str, client: &ApiClient) -> Result<()> {
        if value == "edit" {
            self.prefill_goal(self.goal.as_ref().map(|goal| goal.goal.clone()));
            return Ok(());
        }
        let action = match value {
            "current" | "status" => GoalActionRequest::Status,
            "pause" => GoalActionRequest::Pause,
            "resume" => GoalActionRequest::Resume,
            "clear" => {
                self.open_goal_clear_confirmation();
                return Ok(());
            }
            _ => return Ok(()),
        };
        let response = match client.goal_action(self.thread.id, &action).await {
            Ok(response) => response,
            Err(error) => {
                self.open_goal_overlay();
                return Err(error);
            }
        };
        self.install_goal_response(response);
        if matches!(action, GoalActionRequest::Clear) {
            self.close_overlay();
        } else {
            self.open_goal_overlay();
        }
        Ok(())
    }

    async fn execute_goal_clear_confirmation(
        &mut self,
        value: &str,
        client: &ApiClient,
    ) -> Result<()> {
        if value != "confirm" {
            self.open_goal_overlay();
            return Ok(());
        }
        let response = match client
            .goal_action(self.thread.id, &GoalActionRequest::Clear)
            .await
        {
            Ok(response) => response,
            Err(error) => {
                self.open_goal_overlay();
                return Err(error);
            }
        };
        self.install_goal_response(response);
        self.last_goal_completion = None;
        self.close_overlay();
        Ok(())
    }

    async fn execute_goal_completion_choice(
        &mut self,
        value: &str,
        client: &ApiClient,
    ) -> Result<()> {
        match value {
            "edit" => self.prefill_goal(self.goal.as_ref().map(|goal| goal.goal.clone())),
            "new" => self.prefill_goal(None),
            "clear" => {
                let completion = self.goal_completion.clone();
                let response = match client
                    .goal_action(self.thread.id, &GoalActionRequest::Clear)
                    .await
                {
                    Ok(response) => response,
                    Err(error) => {
                        if let Some(completion) = completion {
                            self.open_goal_completion(completion);
                        }
                        return Err(error);
                    }
                };
                self.install_goal_response(response);
                self.close_overlay();
            }
            _ => self.close_overlay(),
        }
        self.goal_completion = None;
        Ok(())
    }

    fn prefill_goal(&mut self, objective: Option<String>) {
        self.close_overlay();
        self.composer.set_text(match objective {
            Some(objective) if !objective.is_empty() => format!("/goal {objective}"),
            _ => "/goal ".into(),
        });
        self.notice = "Edit the objective, then press Enter".into();
    }

    /// Why a reasoning effort cannot be chosen, naming the model as the
    /// picker does and the efforts it does have.
    fn unsupported_effort(&self, effort: &str) -> String {
        let model = self.model_label(&format!("{}/{}", self.thread.provider, self.thread.model));
        match self.reasoning_efforts() {
            [] => format!("{model} does not support reasoning effort"),
            [only] => format!("{model} does not support {effort} reasoning · it offers {only}"),
            [rest @ .., last] => format!(
                "{model} does not support {effort} reasoning · it offers {} or {last}",
                rest.join(", ")
            ),
        }
    }

    async fn set_reasoning_option(&mut self, value: &str, client: &ApiClient) -> Result<()> {
        if !matches!(value, "off" | "ultracode" | "display")
            && !self
                .reasoning_efforts()
                .iter()
                .any(|effort| effort == value)
        {
            self.notice = self.unsupported_effort(value);
            return Ok(());
        }
        if value != "display" {
            let enabled = value == "ultracode";
            if self.thread.ultracode != enabled {
                self.thread = client
                    .update_thread(
                        self.thread.id,
                        UpdateThreadRequest {
                            title: None,
                            archived: None,
                            agent: None,
                            provider: None,
                            model: None,
                            ultracode: Some(enabled),
                        },
                    )
                    .await?;
            }
        }
        self.apply_option(OptionsKind::ReasoningEffort, value);
        Ok(())
    }

    async fn set_ultracode(&mut self, enabled: bool, client: &ApiClient) -> Result<()> {
        self.thread = client
            .update_thread(
                self.thread.id,
                UpdateThreadRequest {
                    title: None,
                    archived: None,
                    agent: None,
                    provider: None,
                    model: None,
                    ultracode: Some(enabled),
                },
            )
            .await?;
        if self.thread.ultracode {
            self.reasoning_effort = None;
            self.notice =
                "Ultracode enabled · task orchestration · strongest compatible reasoning".into();
        } else {
            self.notice = "Ultracode disabled · reasoning uses provider default".into();
        }
        Ok(())
    }

    async fn execute_workflow_command(
        &mut self,
        arguments: &str,
        client: &ApiClient,
    ) -> Result<()> {
        let command = match parse_workflow_command(arguments) {
            Ok(command) => command,
            Err(message) => {
                self.notice = message;
                return Ok(());
            }
        };
        match command {
            WorkflowSlashCommand::Dashboard => self.refresh_workflow_dashboard(client).await,
            WorkflowSlashCommand::Run { name, args } => {
                self.start_named_workflow(name, args, client).await
            }
            WorkflowSlashCommand::Inspect(id) => {
                let Some(run) = self.resolve_workflow_run(&id, client).await? else {
                    return Ok(());
                };
                self.open_workflow_run_details(&run);
                Ok(())
            }
            WorkflowSlashCommand::Cancel(id) => {
                self.apply_workflow_action("cancel", &id, client).await
            }
            WorkflowSlashCommand::Pause(id) => {
                self.apply_workflow_action("pause", &id, client).await
            }
            WorkflowSlashCommand::Resume(id) => {
                self.apply_workflow_action("resume", &id, client).await
            }
            WorkflowSlashCommand::Delete(id) => {
                self.apply_workflow_action("delete", &id, client).await
            }
        }
    }

    async fn refresh_workflow_dashboard(&mut self, client: &ApiClient) -> Result<()> {
        let (catalog, runs) = tokio::try_join!(client.workflows(), client.workflow_runs(50))?;
        let invalid = catalog.invalid.len();
        self.open_workflow_dashboard(catalog, runs);
        self.notice = if invalid == 0 {
            "Workflows · recent runs and reusable definitions".into()
        } else {
            format!("Workflows · {invalid} invalid definition(s) need attention")
        };
        Ok(())
    }

    async fn start_named_workflow(
        &mut self,
        name: String,
        mut args: Option<serde_json::Value>,
        client: &ApiClient,
    ) -> Result<()> {
        if let Some(definition) = self
            .workflow_definitions
            .iter()
            .find(|definition| definition.invocation.eq_ignore_ascii_case(&name))
        {
            args = Some(workflow_command_args(definition, args).map_err(anyhow::Error::msg)?);
        }
        let run = client
            .start_workflow(WorkflowStartRequest {
                name: Some(name),
                source: None,
                path: None,
                args,
                agent: Some(self.agent().into()),
                model: Some(workflow_model(&self.thread.provider, &self.thread.model)),
                variant: self.reasoning_effort.clone(),
                thread_id: Some(self.thread.id),
                wait: false,
                timeout_ms: None,
            })
            .await?;
        self.upsert_workflow_run(run.clone());
        // A chat opened only to run a workflow would otherwise stay "New
        // chat" forever, as nothing in it is a prompt to title it from.
        if haarness_protocol::is_untitled_chat_title(&self.thread.title) && !self.thread.is_subagent
        {
            let request = UpdateThreadRequest {
                title: Some(format!("Workflow · {}", run.workflow_name)),
                archived: None,
                agent: None,
                provider: None,
                model: None,
                ultracode: None,
            };
            if let Ok(thread) = client.update_thread(self.thread.id, request).await {
                if let Some(listed) = self.threads.iter_mut().find(|t| t.id == thread.id) {
                    listed.title.clone_from(&thread.title);
                }
                self.thread.title = thread.title;
            }
        }
        self.notice = format!(
            "Workflow {} started · {}",
            run.workflow_name,
            crate::view::span::short_id(&run.id.to_string())
        );
        self.open_workflow_run_actions(&run);
        Ok(())
    }

    async fn execute_workflow_dashboard_choice(
        &mut self,
        value: &str,
        client: &ApiClient,
    ) -> Result<()> {
        let Some((kind, target)) = value.split_once(':') else {
            self.notice = "No workflow selected".into();
            return Ok(());
        };
        match kind {
            "definition" => {
                self.start_named_workflow(target.to_owned(), None, client)
                    .await
            }
            "run" => {
                let Some(run) = self.resolve_workflow_run(target, client).await? else {
                    return Ok(());
                };
                self.open_workflow_run_actions(&run);
                Ok(())
            }
            "invalid" => {
                let invalid = target
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| self.workflow_invalid.get(index))
                    .cloned();
                if let Some(invalid) = invalid {
                    self.open_overlay(Overlay::Panel {
                        title: "Invalid workflow".into(),
                        lines: vec![
                            format!("path  {}", invalid.path),
                            format!("error  {}", invalid.error),
                        ],
                    });
                } else {
                    self.notice = "Invalid workflow definition is no longer available".into();
                }
                Ok(())
            }
            _ => {
                self.notice = "No workflows discovered".into();
                Ok(())
            }
        }
    }

    async fn execute_workflow_run_action(&mut self, value: &str, client: &ApiClient) -> Result<()> {
        let Some((action, id)) = value.split_once(':') else {
            self.notice = "Unknown workflow action".into();
            return Ok(());
        };
        if action == "copy" {
            crate::terminal::copy_to_clipboard(id);
            self.notice = "Copied workflow run ID".into();
            return Ok(());
        }
        match action {
            "inspect" => {
                let Some(run) = self.resolve_workflow_run(id, client).await? else {
                    return Ok(());
                };
                self.open_workflow_run_details(&run);
                Ok(())
            }
            "session" => {
                let Some(run) = self.resolve_workflow_run(id, client).await? else {
                    return Ok(());
                };
                self.enter_workflow_session(&run, client).await?;
                Ok(())
            }
            "launch" => {
                let Some(run) = self.resolve_workflow_run(id, client).await? else {
                    return Ok(());
                };
                self.enter_workflow_launch(&run, client).await?;
                Ok(())
            }
            "pause" | "resume" | "cancel" | "delete" => {
                self.apply_workflow_action(action, id, client).await
            }
            _ => {
                self.notice = format!("Unknown workflow action {action}");
                Ok(())
            }
        }
    }

    async fn apply_workflow_action(
        &mut self,
        action: &str,
        raw_id: &str,
        client: &ApiClient,
    ) -> Result<()> {
        let Some(run) = self.resolve_workflow_run(raw_id, client).await? else {
            return Ok(());
        };
        let id = run.id;
        if action == "delete" {
            client.delete_workflow(id).await?;
            self.workflow_runs.retain(|candidate| candidate.id != id);
            self.refresh_workflow_dashboard(client).await?;
            self.notice = format!(
                "Deleted workflow run {}",
                crate::view::span::short_id(&id.to_string())
            );
            return Ok(());
        }
        let updated = match action {
            "pause" => client.pause_workflow(id).await?,
            "resume" => client.resume_workflow(id).await?,
            "cancel" => client.cancel_workflow(id).await?,
            _ => {
                self.notice = format!("Unknown workflow action {action}");
                return Ok(());
            }
        };
        self.upsert_workflow_run(updated.clone());
        self.refresh_workflow_dashboard(client).await?;
        self.notice = format!(
            "Workflow {} {}",
            updated.workflow_name,
            match action {
                "pause" => "paused",
                "resume" => "resumed",
                "cancel" => "cancelled",
                _ => action,
            }
        );
        Ok(())
    }

    async fn resolve_workflow_run(
        &mut self,
        raw_id: &str,
        client: &ApiClient,
    ) -> Result<Option<WorkflowRun>> {
        let id = if let Ok(id) = raw_id.parse::<uuid::Uuid>() {
            WorkflowId(id)
        } else {
            let runs = client.workflow_runs(50).await?;
            self.workflow_runs.clone_from(&runs);
            let mut matches = runs
                .iter()
                .filter(|run| run.id.to_string().starts_with(raw_id));
            let first = matches.next().map(|run| run.id);
            if first.is_none() {
                self.notice = format!("Workflow run {raw_id} was not found");
                return Ok(None);
            }
            if matches.next().is_some() {
                self.notice = format!("Workflow run ID {raw_id} is ambiguous");
                return Ok(None);
            }
            first.expect("checked above")
        };
        let run = client.workflow_run(id, None).await?;
        self.upsert_workflow_run(run.clone());
        Ok(Some(run))
    }

    /// Resolve a workflow row from the transcript and expose its authoritative
    /// lifecycle/actions without making the user copy an ID into /workflow.
    pub async fn open_workflow_run(&mut self, id: WorkflowId, client: &ApiClient) -> Result<()> {
        let run = client.workflow_run(id, None).await?;
        self.upsert_workflow_run(run.clone());
        self.open_workflow_run_actions(&run);
        Ok(())
    }

    /// The thread as portable markdown for /export.
    #[must_use]
    pub fn transcript_markdown(&self) -> String {
        use std::fmt::Write as _;
        let mut out = format!("# {}\n\n", self.thread.title);
        for item in &self.items {
            match &item.data {
                haarness_protocol::ItemData::UserMessage {
                    text,
                    synthetic: false,
                    ..
                } => {
                    let _ = write!(out, "## User\n\n{text}\n\n");
                }
                haarness_protocol::ItemData::AssistantMessage { text } => {
                    let _ = write!(out, "## Assistant\n\n{text}\n\n");
                }
                haarness_protocol::ItemData::ToolCall {
                    name, arguments, ..
                } => {
                    let _ = write!(out, "> tool `{name}` {arguments}\n\n");
                }
                haarness_protocol::ItemData::ToolResult {
                    output, is_error, ..
                } => {
                    let marker = if *is_error { "error" } else { "output" };
                    let _ = write!(out, "```{marker}\n{output}\n```\n\n");
                }
                _ => {}
            }
        }
        out
    }

    /// Execute one command exactly as advertised by the shared server
    /// registry. Workflow responses retain their durable run identity while
    /// configured prompts preserve the normal active-turn UI lifecycle.
    async fn submit_server_command(
        &mut self,
        command: &haarness_protocol::CommandInfo,
        arguments: &str,
        client: &ApiClient,
    ) -> Result<()> {
        let workflow = command.source.as_deref() == Some("workflow");
        if !workflow && self.active_turn.is_some() {
            self.notice = "A turn is already running".into();
            return Ok(());
        }
        let arguments = if workflow {
            parse_workflow_args(arguments).map_err(anyhow::Error::msg)?
        } else if arguments.is_empty() {
            None
        } else {
            Some(serde_json::Value::String(arguments.to_owned()))
        };
        let response = client
            .execute_command(
                self.thread.id,
                ExecuteCommandRequest {
                    request_id: RequestId::new(),
                    name: command.name.clone(),
                    arguments,
                    attachments: if workflow {
                        Vec::new()
                    } else {
                        self.attachments.clone()
                    },
                    agent: workflow.then(|| self.agent().to_owned()),
                    provider: workflow.then(|| self.thread.provider.clone()),
                    model: workflow.then(|| self.thread.model.clone()),
                    variant: workflow.then(|| self.reasoning_effort.clone()).flatten(),
                    reasoning_effort: (!workflow).then(|| self.reasoning_effort.clone()).flatten(),
                    tools: None,
                    wait: false,
                    timeout_ms: None,
                },
            )
            .await?;
        match response {
            CommandExecutionResponse::Turn { turn } => {
                self.attachments.clear();
                self.active_turn = Some(turn.id);
                if self.thread.id == self.root_thread_id {
                    self.root_active_turn = Some(turn.id);
                }
                self.preview.clear();
                self.turn_started = Some(std::time::Instant::now());
                self.refresh_needed = true;
                self.notice = format!("/{} running…", command.name);
            }
            CommandExecutionResponse::WorkflowRun { run } => {
                self.upsert_workflow_run((*run).clone());
                self.notice = format!(
                    "Workflow {} · {}",
                    run.workflow_name,
                    crate::view::span::short_id(&run.id.to_string())
                );
                self.open_workflow_run_actions(&run);
            }
            CommandExecutionResponse::Text { text } => {
                self.notice = text.lines().collect::<Vec<_>>().join(" · ");
            }
        }
        Ok(())
    }

    pub async fn submit_prompt(&mut self, text: String, client: &ApiClient) -> Result<bool> {
        self.normalize_reasoning();
        if self.active_turn.is_some() {
            self.notice = "A turn is already running".into();
            return Ok(false);
        }
        if !self
            .providers
            .iter()
            .any(|provider| provider.id == self.thread.provider && provider.authenticated)
        {
            if !self.any_provider_connected() {
                // Nothing can answer yet, and the chat's default provider is
                // not a choice the person made: open the picker and keep the
                // draft rather than name a provider they never picked.
                self.connect.onboarding = true;
                self.open_connect_picker(None, client).await?;
                self.notice = "Connect a provider to send your message".into();
                return Ok(false);
            }
            self.notice = crate::state::not_connected(&self.providers, &self.thread.provider);
            return Ok(false);
        }
        let attachments = self.attachments.clone();
        let turn = client
            .start_turn(
                self.thread.id,
                StartTurnRequest {
                    request_id: RequestId::new(),
                    text,
                    attachments,
                    synthetic: false,
                    auto_approve: false,
                    agent: Some(self.agent().into()),
                    provider: None,
                    model: None,
                    reasoning_effort: self.reasoning_effort.clone(),
                    // A terminal shows no file previews; saying so keeps a
                    // Desktop chat's preview tool out of terminal turns.
                    tools: Some(std::collections::HashMap::from([("preview".into(), false)])),
                    command: None,
                },
            )
            .await?;
        self.attachments.clear();
        self.active_turn = Some(turn.id);
        self.preview.clear();
        self.reasoning_preview.clear();
        self.live_threads.remove(&self.thread.id);
        self.turn_started = Some(std::time::Instant::now());
        self.notice = "Thinking…".into();
        Ok(true)
    }

    pub async fn switch_thread(&mut self, thread: Thread, client: &ApiClient) -> Result<()> {
        let previous_thread = self.thread.id;
        let abandoned = (thread.id != previous_thread && self.is_untouched_session())
            .then_some(previous_thread);
        // An unsaved queued-message edit belongs to the session being left;
        // its set-aside draft is what that session keeps.
        self.cancel_queue_edit();
        // A prompt `/undo` restored stays that chat's draft, even when
        // the command that leaves it (`/new`) was typed over it.
        self.return_undo_draft();
        self.undo_draft = None;
        if self.composer.is_empty() && self.attachments.is_empty() {
            self.drafts.remove(&previous_thread);
        } else {
            self.drafts
                .insert(previous_thread, crate::servers::Draft::from_app(self));
        }
        // A child is an inspector, never a prompt target. Remove any draft
        // left by an older client, but restore drafts for ordinary sessions and forks.
        let restored_draft = self
            .drafts
            .remove(&thread.id)
            .filter(|_| !thread.is_subagent);
        let previous_root = self.root_thread_id;
        self.items = client.items(thread.id).await?;
        let todos = client.todos(thread.id).await.ok();
        self.agent_index = self
            .agents
            .iter()
            .position(|agent| agent.id == thread.agent)
            .unwrap_or(0);
        self.thread = thread;
        if let Some(id) = abandoned {
            archive_untouched(
                &mut self.threads,
                id,
                client,
                std::time::Duration::from_secs(1),
            )
            .await;
        }
        self.input_queue = client.input_queue(self.thread.id).await.unwrap_or_default();
        self.input_queue_edit = None;
        self.normalize_reasoning();
        if let Some(todos) = todos {
            self.todos = todos;
        } else {
            self.hydrate_todos_from_items();
        }
        self.attachments.clear();
        self.composer.clear();
        self.shell_mode = false;
        if let Some(draft) = restored_draft {
            self.composer.set_text(draft.text);
            self.attachments = draft.attachments;
            self.shell_mode = draft.shell;
        }
        let live = self
            .live_threads
            .get(&self.thread.id)
            .cloned()
            .unwrap_or_default();
        self.preview = live.preview;
        self.reasoning_preview = live.reasoning_preview;
        self.phase = live.phase;
        self.viewport_offset = 0;
        self.scroll_adjustment.set(0);
        self.last_transcript_len.set(0);
        self.mention_suggestions.clear();
        self.selection = 0;
        self.selection_range = None;
        self.mouse_down = None;
        self.hovered_cell = None;
        self.close_overlay();
        self.parent_stack.clear();
        self.escape_interrupt = None;
        self.ctrl_c_action = None;
        self.root_thread_id = crate::state::compute_root(&self.threads, self.thread.id);
        if !self.is_child_view() {
            if self.root_thread_id != previous_root {
                self.children.clear();
            }
            self.hydrate_child_links();
        }
        if let Ok(turns) = client.active_turns().await {
            self.hydrate_active_turns(turns);
        } else {
            // Compatibility with an older server retains per-thread attach
            // behavior without carrying the previous root's activity flag.
            self.active_turn = client
                .active_turn(self.thread.id)
                .await
                .unwrap_or(None)
                .map(|turn| turn.id);
            self.root_active_turn = if self.thread.id == self.root_thread_id {
                self.active_turn
            } else {
                None
            };
            self.turn_started = self.active_turn.map(|_| std::time::Instant::now());
        }
        self.goal = self.goal_snapshots.get(&self.thread.id).cloned().flatten();
        self.goal_completion = self
            .goal
            .as_ref()
            .is_some_and(|goal| goal.status == GoalStatus::Complete)
            .then(default_goal_completion);
        crate::hydrate_goal_snapshot(self, client).await;
        self.hydrate_questions(client.pending_questions().await.unwrap_or_default());
        // Repopulate pending approvals: live SSE alone cannot restore prompts
        // that were raised before this view attached to the thread.
        self.hydrate_permissions(
            client.pending_permissions().await.unwrap_or_default(),
            client,
        )
        .await;
        if self.pending_permissions.is_empty() {
            self.permission_focus = false;
            self.reveal_goal_completion();
        } else {
            self.permission_focus = self.composer.is_empty();
            self.update_permission_notice();
            return Ok(());
        }
        self.notice = if self.active_turn.is_some() {
            "Running — attached to server task".into()
        } else if self
            .providers
            .iter()
            .any(|provider| provider.id == self.thread.provider && provider.authenticated)
        {
            "Ready".into()
        } else {
            crate::state::unconnected_notice(&self.providers, &self.thread.provider)
        };
        Ok(())
    }

    /// Send input while a turn is running: the server queues it and steers it
    /// into the running turn's next provider round.
    pub async fn queue_prompt(&mut self, text: String, client: &ApiClient) -> Result<bool> {
        let attachments = self.attachments.clone();
        match client
            .queue_input(
                self.thread.id,
                haarness_protocol::QueueInputRequest {
                    preview: Some(false),
                    reasoning_effort: self.reasoning_effort.clone(),
                    delivery: haarness_protocol::InputDelivery::Steer,
                    request_id: haarness_protocol::RequestId::new(),
                    text,
                    attachments,
                    synthetic: false,
                },
            )
            .await
        {
            Ok(response) if response.queued => {
                self.attachments.clear();
                self.refresh_needed = true;
                self.notice = "Follow-up sent — the agent will address it next".into();
                Ok(true)
            }
            Ok(response) => {
                self.attachments.clear();
                // The turn ended while we typed; a fresh turn started.
                if let Some(turn) = response.turn {
                    self.active_turn = Some(turn.id);
                    if self.thread.id == self.root_thread_id {
                        self.root_active_turn = Some(turn.id);
                    }
                    self.turn_started = Some(std::time::Instant::now());
                    self.notice = "Thinking…".into();
                }
                Ok(true)
            }
            Err(error) => Err(error),
        }
    }

    /// Overlay listing live child-agent tasks; choosing one opens its thread.
    pub fn open_tasks_overlay(&mut self) {
        let mut tasks = self.visible_children();
        if tasks.is_empty() {
            self.notice = "No child agents in this turn".into();
            return;
        }
        tasks.sort_by(|a, b| a.0.cmp(b.0));
        let mut entries = Vec::with_capacity(tasks.len());
        for (call_id, task) in tasks {
            let (title, agent) = self.child_task_identity(call_id, task);
            // Lifecycle comes first so a narrow picker never truncates the one
            // fact the user needs to distinguish live, blocked, and finished
            // work. Identity stays in the label; activity follows as context.
            let state = match task.state.as_str() {
                "waiting_permission" => "needs approval".into(),
                "done" => "completed".into(),
                "failed" => "failed".into(),
                "cancelled" | "interrupted" => "cancelled".into(),
                "running" => task.elapsed_seconds().map_or_else(
                    || "running".into(),
                    |seconds| format!("running · {seconds}s"),
                ),
                "background" => task.elapsed_seconds().map_or_else(
                    || "running in background".into(),
                    |seconds| format!("running in background · {seconds}s"),
                ),
                state => state.to_owned(),
            };
            let mut detail = vec![state];
            if title != agent {
                detail.push(agent);
            }
            if let Some(tool) = task.last_tool.as_deref() {
                detail.push(if task.is_active() {
                    format!("activity: {tool}")
                } else {
                    format!("last activity: {tool}")
                });
            }
            detail.push(format!("{} tool calls", task.tool_calls));
            detail.push("enter opens the child".into());
            entries.push(OptionEntry {
                label: title,
                description: detail.join(" · "),
                value: task.child_thread_id.to_string(),
            });
        }
        self.open_overlay(Overlay::Options {
            title: "child agents".into(),
            kind: OptionsKind::Tasks,
            entries,
        });
    }

    /// Drill into a child-agent thread; Up returns to the parent.
    pub async fn enter_child(
        &mut self,
        child_id: haarness_protocol::ThreadId,
        client: &ApiClient,
    ) -> Result<()> {
        if !self.threads.iter().any(|thread| thread.id == child_id) {
            self.refresh_threads(client).await?;
        }
        let Some(child) = self
            .threads
            .iter()
            .find(|thread| thread.id == child_id)
            .cloned()
        else {
            self.notice = "Child thread not found".into();
            return Ok(());
        };
        let parent = self.thread.clone();
        self.switch_thread(child, client).await?;
        self.parent_stack.push(parent);
        // A still-running child keeps its live activity visible.
        if let Some(task) = self
            .children
            .values()
            .find(|task| task.child_thread_id == child_id)
        {
            if task.is_active() {
                self.active_turn = task.child_turn_id;
                self.turn_started = Some(std::time::Instant::now());
            }
        }
        self.notice = "Child agent view — ↑ parent · ←/→ siblings".into();
        Ok(())
    }

    /// Open a workflow's dedicated execution thread. The dashboard is global,
    /// so its run may belong to a different conversation than the one
    /// currently open; anchor Up navigation to the recorded launching thread
    /// instead of whichever session happened to browse the run.
    pub async fn enter_workflow_session(
        &mut self,
        run: &WorkflowRun,
        client: &ApiClient,
    ) -> Result<()> {
        let Some(child_id) = run.thread_id else {
            self.notice = "This workflow has no inspectable session".into();
            return Ok(());
        };
        let launch_id = run.definition.launch_thread_id;
        if launch_id == Some(self.thread.id) {
            return self.enter_child(child_id, client).await;
        }
        if !self.threads.iter().any(|thread| thread.id == child_id)
            || launch_id.is_some_and(|id| !self.threads.iter().any(|thread| thread.id == id))
        {
            self.refresh_threads(client).await?;
        }
        let Some(child) = self
            .threads
            .iter()
            .find(|thread| thread.id == child_id)
            .cloned()
        else {
            self.notice = "Workflow session not found".into();
            return Ok(());
        };
        let parent_id = launch_id.or(child.parent_id);
        let parent =
            parent_id.and_then(|id| self.threads.iter().find(|thread| thread.id == id).cloned());
        self.switch_thread(child, client).await?;
        // `switch_thread` deliberately retains root task metadata while
        // moving among normal children. A foreign workflow root must not
        // inherit sibling controls from the previously open conversation.
        self.children.clear();
        if let Some(parent) = parent {
            self.parent_stack.push(parent);
            self.notice = "Workflow session · ↑ launching conversation".into();
        } else {
            self.notice = "Workflow session".into();
        }
        Ok(())
    }

    /// Navigate from a directory-wide workflow result back to the exact task
    /// that launched it. Child-launched workflows therefore remain attached
    /// to the agent hierarchy instead of becoming orphan-looking global runs.
    pub async fn enter_workflow_launch(
        &mut self,
        run: &WorkflowRun,
        client: &ApiClient,
    ) -> Result<()> {
        let Some(launch_id) = run.definition.launch_thread_id else {
            self.notice = "This legacy workflow has no recorded launching task".into();
            return Ok(());
        };
        if !self.threads.iter().any(|thread| thread.id == launch_id) {
            self.refresh_threads(client).await?;
        }
        let Some(launch) = self
            .threads
            .iter()
            .find(|thread| thread.id == launch_id)
            .cloned()
        else {
            self.notice = "Launching task was not found".into();
            return Ok(());
        };
        let parent = launch.parent_id.and_then(|parent_id| {
            self.threads
                .iter()
                .find(|thread| thread.id == parent_id)
                .cloned()
        });
        self.switch_thread(launch, client).await?;
        if let Some(parent) = parent {
            self.parent_stack.push(parent);
            self.notice = "Workflow launch task · ↑ parent".into();
        } else {
            self.notice = "Workflow launching conversation".into();
        }
        Ok(())
    }

    /// Pop back out of a child-agent view.
    pub async fn leave_child(&mut self, client: &ApiClient) -> Result<()> {
        let Some(parent_snapshot) = self.parent_stack.pop() else {
            return Ok(());
        };
        let parent = self
            .threads
            .iter()
            .find(|thread| thread.id == parent_snapshot.id)
            .cloned()
            .unwrap_or(parent_snapshot);
        let stack = std::mem::take(&mut self.parent_stack);
        let result = self.switch_thread(parent, client).await;
        self.parent_stack = stack;
        result?;
        Ok(())
    }

    /// Move between child-agent threads without growing or losing the parent
    /// breadcrumb stack.
    pub async fn cycle_child(&mut self, direction: isize, client: &ApiClient) -> Result<()> {
        if self.parent_stack.is_empty() || self.children.is_empty() {
            return Ok(());
        }
        let mut siblings: Vec<_> = self
            .children
            .iter()
            .map(|(call_id, task)| (call_id.clone(), task.child_thread_id))
            .collect();
        siblings.sort_by(|left, right| left.0.cmp(&right.0));
        let Some(current) = siblings
            .iter()
            .position(|(_, thread_id)| *thread_id == self.thread.id)
        else {
            return Ok(());
        };
        let next = if direction < 0 {
            current.checked_sub(1).unwrap_or(siblings.len() - 1)
        } else {
            (current + 1) % siblings.len()
        };
        if next == current {
            return Ok(());
        }
        let thread_id = siblings[next].1;
        if !self.threads.iter().any(|thread| thread.id == thread_id) {
            self.refresh_threads(client).await?;
        }
        let Some(thread) = self
            .threads
            .iter()
            .find(|thread| thread.id == thread_id)
            .cloned()
        else {
            self.notice = "Sibling child thread not found".into();
            return Ok(());
        };
        let stack = std::mem::take(&mut self.parent_stack);
        let result = self.switch_thread(thread, client).await;
        self.parent_stack = stack;
        result?;
        self.notice = "Child agent view — ↑ parent · ←/→ siblings".into();
        Ok(())
    }

    async fn open_sessions(&mut self, arguments: &str, client: &ApiClient) -> Result<()> {
        let (include_archived, query) = if matches!(arguments, "--all" | "-a") {
            (true, "")
        } else if let Some(query) = arguments
            .strip_prefix("--all ")
            .or_else(|| arguments.strip_prefix("-a "))
        {
            (true, query.trim())
        } else {
            (false, arguments.trim())
        };
        self.refresh_session_threads(include_archived, client)
            .await?;

        if !query.is_empty() {
            query.clone_into(&mut self.overlay_filter);
            let candidates = self.filtered_threads();
            let exact = candidates.iter().find(|thread| {
                thread.id.to_string().eq_ignore_ascii_case(query)
                    || thread.title.eq_ignore_ascii_case(query)
            });
            let target = exact
                .copied()
                .or_else(|| (candidates.len() == 1).then(|| candidates[0]))
                .cloned();
            if let Some(thread) = target {
                self.overlay_filter.clear();
                self.resume_session(thread, client).await?;
                return Ok(());
            }
        }

        self.open_overlay(Overlay::Threads);
        if !query.is_empty() {
            query.clone_into(&mut self.overlay_filter);
            self.selection = 0;
        }
        Ok(())
    }

    async fn execute_session_action(&mut self, value: &str, client: &ApiClient) -> Result<()> {
        let Some((action, raw_id)) = value.split_once(':') else {
            self.notice = "Unknown session action".into();
            return Ok(());
        };
        let Ok(id) = raw_id.parse::<uuid::Uuid>() else {
            self.notice = "Invalid session ID".into();
            return Ok(());
        };
        let id = haarness_protocol::ThreadId(id);
        let Some(mut thread) = self.threads.iter().find(|thread| thread.id == id).cloned() else {
            self.notice = "Session is no longer available".into();
            return Ok(());
        };

        match action {
            "resume" => {
                self.resume_session(thread, client).await?;
            }
            "fork" => {
                let forked = client.fork_thread(id).await?;
                self.upsert_thread(forked.clone());
                self.switch_thread(forked, client).await?;
            }
            "rename" => {
                let short_id = crate::view::span::short_id(raw_id);
                self.composer.set_text(format!("/rename {short_id} "));
                self.notice = format!("Rename {} — type a title and press Enter", thread.title);
            }
            "archive" if id == self.thread.id => {
                self.archive_current_session(client).await?;
            }
            "archive" | "unarchive" => {
                let archived = action == "archive";
                thread = client
                    .update_thread(
                        id,
                        UpdateThreadRequest {
                            title: None,
                            archived: Some(archived),
                            agent: None,
                            provider: None,
                            model: None,
                            ultracode: None,
                        },
                    )
                    .await?;
                if archived && !self.session_include_archived {
                    self.threads.retain(|candidate| candidate.id != id);
                } else {
                    self.upsert_thread(thread);
                }
                self.notice = if archived {
                    "Session archived — use /sessions --all to restore it".into()
                } else {
                    "Session restored".into()
                };
            }
            "copy" => {
                crate::terminal::copy_to_clipboard(raw_id);
                self.notice = "Session ID copied".into();
            }
            _ => self.notice = "Unknown session action".into(),
        }
        Ok(())
    }

    async fn resume_session(&mut self, mut thread: Thread, client: &ApiClient) -> Result<()> {
        if thread.archived {
            thread = client
                .update_thread(
                    thread.id,
                    UpdateThreadRequest {
                        title: None,
                        archived: Some(false),
                        agent: None,
                        provider: None,
                        model: None,
                        ultracode: None,
                    },
                )
                .await?;
            self.upsert_thread(thread.clone());
        }
        self.switch_thread(thread, client).await
    }

    fn rename_target<'a>(&self, arguments: &'a str) -> (haarness_protocol::ThreadId, &'a str) {
        let Some((candidate, title)) = arguments.split_once(char::is_whitespace) else {
            return (self.thread.id, arguments);
        };
        let title = title.trim();
        if title.is_empty() {
            return (self.thread.id, arguments);
        }
        self.matching_session_id(candidate)
            .map_or((self.thread.id, arguments), |id| (id, title))
    }

    fn matching_session_id(&self, candidate: &str) -> Option<haarness_protocol::ThreadId> {
        let mut matches = self
            .threads
            .iter()
            .filter(|thread| thread.id.to_string().contains(candidate));
        let first = matches.next()?.id;
        matches.next().is_none().then_some(first)
    }

    /// `/billing`: balances on top, and the next step one Enter away instead
    /// of a page of URLs and a line of subcommands to retype.
    pub(crate) fn open_billing(&mut self) {
        let idle_purchase = self.billing.summary.as_ref().is_some_and(|summary| {
            !summary.overage_opt_in
                && haarness_account::billing::display_credits(&summary.purchased_credits) != "0"
        });
        let mut entries = if self.billing.logged_in {
            vec![
                OptionEntry {
                    label: "Add credits".into(),
                    description: "Buy credits in your browser".into(),
                    value: "topup".into(),
                },
                if self.billing.summary.as_ref().is_some_and(|summary| {
                    let plan = summary.plan.trim();
                    !plan.is_empty() && !plan.eq_ignore_ascii_case("no plan")
                }) {
                    OptionEntry {
                        label: "Change plan".into(),
                        description: "Compare plans, upgrade or cancel".into(),
                        value: "open".into(),
                    }
                } else {
                    OptionEntry {
                        label: "Choose a plan".into(),
                        description: "Included credits every month".into(),
                        value: "open".into(),
                    }
                },
                OptionEntry {
                    label: "Usage".into(),
                    description: "What each session and model spent".into(),
                    value: "usage".into(),
                },
            ]
        } else {
            vec![OptionEntry {
                label: "Sign in to Haarness".into(),
                description: "Use Cloud models on your Haarness plan".into(),
                value: "signin".into(),
            }]
        };
        if idle_purchase {
            // Turning overage on is a spending decision with its own
            // confirmation in Cloud, so this opens Billing rather than flip it.
            entries.insert(
                0,
                OptionEntry {
                    label: "Turn on overage".into(),
                    description: "Let purchased credits pay once included ones run out".into(),
                    value: "open".into(),
                },
            );
        }
        self.open_overlay(Overlay::Options {
            title: "Cloud billing".into(),
            kind: OptionsKind::Billing,
            entries,
        });
    }

    /// Open one of the account's billing pages: `open`, `topup` or `usage`.
    fn open_billing_page(&mut self, page: &str) {
        let Some(summary) = &self.billing.summary else {
            self.notice = if self.billing.logged_in {
                "Cloud billing is still loading · try again in a moment".into()
            } else {
                "Not signed in to Haarness Cloud · /connect haarness".into()
            };
            return;
        };
        let url = match page {
            "usage" => summary.usage_url.clone(),
            "topup" => format!("{}#top-up", summary.billing_url),
            _ => summary.billing_url.clone(),
        };
        self.notice = if crate::terminal::open_browser(&url) {
            match page {
                "usage" => "Opened Cloud usage in your browser".into(),
                "topup" => "Opened Cloud credits in your browser".into(),
                _ => "Opened Cloud billing in your browser".into(),
            }
        } else {
            format!("Open {url}")
        };
    }

    async fn archive_current_session(&mut self, client: &ApiClient) -> Result<()> {
        client
            .update_thread(
                self.thread.id,
                UpdateThreadRequest {
                    title: None,
                    archived: Some(true),
                    agent: None,
                    provider: None,
                    model: None,
                    ultracode: None,
                },
            )
            .await?;
        let thread = client
            .create_thread(CreateThreadRequest {
                id: None,
                schedule: None,
                title: None,
                parent_id: None,
                agent: Some(self.agent().into()),
                provider: Some(self.thread.provider.clone()),
                model: Some(self.thread.model.clone()),
                ultracode: None,
            })
            .await?;
        self.refresh_threads(client).await?;
        self.switch_thread(thread, client).await?;
        Ok(())
    }

    fn upsert_thread(&mut self, thread: Thread) {
        if let Some(current) = self
            .threads
            .iter_mut()
            .find(|current| current.id == thread.id)
        {
            *current = thread;
        } else {
            self.threads.push(thread);
        }
    }

    pub async fn refresh_threads(&mut self, client: &ApiClient) -> Result<()> {
        self.refresh_session_threads(false, client).await
    }

    pub async fn refresh_session_threads(
        &mut self,
        include_archived: bool,
        client: &ApiClient,
    ) -> Result<()> {
        self.threads = if include_archived {
            client.threads_including_archived().await?
        } else {
            client.threads().await?
        };
        self.session_include_archived = include_archived;
        Ok(())
    }

    pub async fn set_model(&mut self, model: &str, client: &ApiClient) -> Result<()> {
        let Some((provider, model)) = model.split_once('/') else {
            self.notice = "Use /models or /models provider/model".into();
            return Ok(());
        };
        let provider = haarness_protocol::provider_storage_id(provider);
        // An unconnected provider lists no models, so check the connection
        // first: its model is not unknown, it is waiting for /connect.
        if self
            .providers
            .iter()
            .any(|candidate| candidate.id == provider && !candidate.authenticated)
        {
            self.notice = crate::state::not_connected(&self.providers, provider);
            return Ok(());
        }
        let exists = self.providers.iter().any(|candidate| {
            candidate.id == provider && candidate.models.iter().any(|value| value.id == model)
        });
        if !exists {
            self.notice = format!("Unknown model {provider}/{model}");
            return Ok(());
        }
        self.thread = client
            .update_thread(
                self.thread.id,
                UpdateThreadRequest {
                    title: None,
                    archived: None,
                    agent: None,
                    provider: Some(provider.into()),
                    model: Some(model.into()),
                    ultracode: None,
                },
            )
            .await?;
        self.remember_model();
        self.notice = format!(
            "Model: {}/{model}",
            haarness_protocol::provider_display_id(provider)
        );
        self.normalize_reasoning();
        Ok(())
    }

    pub async fn set_agent(&mut self, agent: &str, client: &ApiClient) -> Result<()> {
        let Some(index) = self
            .agents
            .iter()
            .position(|candidate| candidate.id == agent && candidate.selectable())
        else {
            self.notice = format!("Unknown agent {agent}");
            return Ok(());
        };
        self.thread = client
            .update_thread(
                self.thread.id,
                UpdateThreadRequest {
                    title: None,
                    archived: None,
                    agent: Some(agent.into()),
                    provider: None,
                    model: None,
                    ultracode: None,
                },
            )
            .await?;
        self.agent_index = index;
        self.notice = format!("Agent: {agent}");
        Ok(())
    }
}

/// `30s` / `5m` / `2h` → seconds.
fn parse_interval(value: &str) -> Option<u64> {
    let (number, unit) = value.split_at(value.len().checked_sub(1)?);
    let number: u64 = number.parse().ok()?;
    let seconds = match unit {
        "s" => number,
        "m" => number * 60,
        "h" => number * 3600,
        _ => return None,
    };
    Some(seconds.clamp(30, 86_400))
}

pub(crate) fn goal_tokens(goal: &GoalState) -> String {
    match goal.token_budget {
        Some(budget) => format!("{} / {budget} tokens", goal.tokens_used),
        None if goal.tokens_used > 0 => format!("{} tokens", goal.tokens_used),
        None => "running".into(),
    }
}

pub(crate) fn goal_status_label(goal: &GoalState) -> String {
    match goal.status {
        GoalStatus::Active => format!("pursuing · {}", goal_tokens(goal)),
        // A goal the person paused needs no explanation of its own.
        GoalStatus::Paused => goal
            .paused_reason
            .as_ref()
            .filter(|reason| reason.as_str() != "user-paused")
            .map_or_else(|| "paused".into(), |reason| format!("paused · {reason}")),
        GoalStatus::Blocked => goal
            .last_reason
            .as_ref()
            .map_or_else(|| "blocked".into(), |reason| format!("blocked · {reason}")),
        GoalStatus::UsageLimited => "usage-limited".into(),
        GoalStatus::BudgetLimited => "budget-limited".into(),
        GoalStatus::Complete => format!("achieved · {}", goal_tokens(goal)),
        GoalStatus::Cleared => "cleared".into(),
    }
}

fn single_line_goal(value: &str, max_chars: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max_chars {
        return compact;
    }
    compact
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>()
        + "…"
}

/// What the goal dialog prints above its choices: the objective, and its
/// status with whatever it is waiting for, both in full.
pub(crate) fn goal_preface(goal: Option<&GoalState>) -> Vec<String> {
    goal.map_or_else(Vec::new, |goal| {
        let mut status = goal_status_label(goal);
        if let Some(first) = status.get_mut(..1) {
            first.make_ascii_uppercase();
        }
        vec![goal.goal.clone(), status]
    })
}

pub(crate) fn default_goal_completion() -> haarness_protocol::GoalCompletion {
    use haarness_protocol::GoalCompletionOption;

    haarness_protocol::GoalCompletion {
        title: "Goal achieved — what next?".into(),
        edit: GoalCompletionOption {
            label: "Edit objective".into(),
            description: "Refine the previous goal text and start a new pursuit".into(),
        },
        new: GoalCompletionOption {
            label: "Start fresh goal".into(),
            description: "Type a new objective from scratch".into(),
        },
        clear: GoalCompletionOption {
            label: "Clear goal".into(),
            description: "Remove the standing goal and stop tracking".into(),
        },
        dismiss: GoalCompletionOption {
            label: "Dismiss".into(),
            description: "Close without changes".into(),
        },
    }
}

/// Choices for the /reasoning modal, with the active state annotated.
fn reasoning_entries(app: &App) -> Vec<OptionEntry> {
    let language = app.language;
    // The levels themselves (low, high, max, ultracode) are values typed after
    // /reasoning, so they stay as typed; what they mean is translated.
    let describe = |key: &str, active: bool| {
        if active {
            format!("{} · {}", language.text(key), language.text("active"))
        } else {
            language.text(key).to_owned()
        }
    };
    let mut entries = vec![OptionEntry {
        label: language.text("provider default").into(),
        description: describe(
            "Let the model decide",
            app.reasoning_effort.is_none() && !app.thread.ultracode,
        ),
        value: "off".into(),
    }];
    for effort in app.reasoning_efforts() {
        let description = match effort.as_str() {
            "none" => "No extended thinking",
            "minimal" => "Minimal thinking",
            "low" => "Brief thinking",
            "medium" => "Balanced thinking",
            "high" => "Deep thinking",
            "xhigh" => "Extra-deep thinking",
            "max" => "Maximum thinking",
            _ => "Model-supported thinking level",
        };
        entries.push(OptionEntry {
            label: effort.clone(),
            description: describe(
                description,
                !app.thread.ultracode && app.reasoning_effort.as_deref() == Some(effort.as_str()),
            ),
            value: effort.into(),
        });
    }
    entries.push(OptionEntry {
        label: "ultracode".into(),
        description: describe(
            "Workflow orchestration · strongest compatible reasoning",
            app.thread.ultracode,
        ),
        value: "ultracode".into(),
    });
    entries.push(OptionEntry {
        label: if app.reasoning {
            language.text("hide reasoning text").into()
        } else {
            language.text("show reasoning text").into()
        },
        description: language
            .text("Toggle the thinking display in the transcript")
            .into(),
        value: "display".into(),
    });
    entries
}

fn language_entries(app: &App) -> Vec<OptionEntry> {
    crate::locale::Language::ALL
        .into_iter()
        .map(|(language, code, label)| OptionEntry {
            label: label.into(),
            description: if app.language == language {
                app.language.text("active").into()
            } else {
                code.into()
            },
            value: code.into(),
        })
        .collect()
}

/// What `/language` says once it switches, in the language just chosen.
pub(crate) fn language_notice(language: crate::locale::Language) -> String {
    format!("{}: {}", language.text("Language"), language.code())
}

/// Archive an untouched session the user has moved on from, so launches and
/// `/new` do not leave a trail of empty "New chat" rows. Best effort: on
/// failure the empty session simply stays listed, as it always did.
pub(crate) async fn archive_untouched(
    threads: &mut Vec<Thread>,
    id: haarness_protocol::ThreadId,
    client: &ApiClient,
    patience: std::time::Duration,
) {
    let request = UpdateThreadRequest {
        title: None,
        archived: Some(true),
        agent: None,
        provider: None,
        model: None,
        ultracode: None,
    };
    let archive = client.update_thread(id, request);
    if matches!(tokio::time::timeout(patience, archive).await, Ok(Ok(_))) {
        threads.retain(|thread| thread.id != id);
    }
}
