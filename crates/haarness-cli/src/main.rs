mod account;
mod upgrade;
mod capability_state;
mod marketplace;
mod acp;
mod browser_settings;
mod config_specs;
mod desktop;
mod desktop_hooks;
mod extensions;
mod input_settings;
mod insights;
mod jsonconfig;
mod layout;
mod management;
mod memory_settings;
mod plugins;
mod profile;
mod remote;
mod remote_cloud;
mod router_connection;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};

use fs2::FileExt as _;
#[cfg(test)]
use haarness_core::EventSubscription;
use haarness_core::{
    AgentRuntime, EventBus, McpRegistry, ProviderRegistry, SkillRegistry, ToolRegistry,
};
use haarness_mcp::NativeMcpRegistry;
#[cfg(test)]
use haarness_protocol::GoalUpdatedEvent;
use haarness_protocol::{
    CreateThreadRequest, GoalActionRequest, GoalState, GoalStatus, ItemData, McpConfigureRequest,
    PTY_CAPABILITY, QuestionReply, RequestId, SCHEMA_VERSION, SESSION_ARCHIVE_CAPABILITY,
    ServerInfo, SessionArchive, StartTurnRequest, Thread, ThreadId, TurnId, TurnStatus,
    UpdateThreadRequest, WORKSPACE_HISTORY_CAPABILITY, WorkflowId, WorkflowRun,
    WorkflowStartRequest, WorkspaceCreateRequest, WorkspaceRemoveRequest, WorkspaceResetRequest,
};
use haarness_providers::{
    CopilotOAuthClient, Credential, CredentialStore, NativeProviderRegistry, OpenAiOAuthClient,
    XaiOAuthClient,
};
use haarness_scheduler::{
    CreateSchedule, RunSource as ScheduleRunSource, RunStatus as ScheduleRunStatus, ScheduleRun,
    Scheduler, Trigger as ScheduleTrigger, UpdateSchedule,
};
use haarness_server::{
    FilesystemSkillRegistry, NativeToolRegistry, ServerState, WorkflowRuntimeAdapter,
};
use haarness_store::SqliteStore;
use haarness_workflow_catalog::WorkflowCatalog;
use haarness_workflows::{ExecutorConfig, WorkflowCoordinator, WorkflowExecutor};
use haarness_workspaces::WorkspaceManager;
use opentelemetry::{KeyValue, global, trace::TracerProvider as _};
use opentelemetry_otlp::{Protocol, WithExportConfig as _};
use opentelemetry_sdk::{
    Resource,
    propagation::TraceContextPropagator,
    trace::{Sampler, SdkTracerProvider},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    ffi::OsString,
    fs,
    io::{self, IsTerminal as _, Read as _, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, Stdio},
    sync::Arc,
    time::{Duration, Instant, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
};
use tracing::Instrument as _;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "haarness",
    version,
    about = "Local-first Rust agentic harness",
    long_about = "A modular Rust agent server and OpenTUI client with application-level permissions."
)]
#[allow(clippy::struct_excessive_bools)] // independent global CLI switches, not state flags
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[arg(long, global = true, default_value = ".")]
    workspace: PathBuf,
    /// Override Haarness's data/config/cache root. Primarily used by isolated
    /// installations and native schedule runners.
    #[arg(long, global = true, env = "HAARNESS_DATA_DIR", hide = true)]
    data_dir: Option<PathBuf>,
    /// Compatibility flag. Project configuration is enabled by default.
    #[arg(long, global = true, hide = true)]
    trust_project: bool,
    /// Ignore project configuration and integrations; keep global configuration.
    #[arg(long, global = true, env = "HAARNESS_DISABLE_PROJECT_CONFIG",
        value_parser = clap::builder::BoolishValueParser::new())]
    disable_project_config: bool,
    /// Log verbosity for the haarness crates: trace|debug|info|warn|error.
    /// Overrides HAARNESS_LOG / RUST_LOG. Logs always also land in
    /// <data>/logs/ regardless of this flag.
    #[arg(long, global = true)]
    log_level: Option<String>,
    /// Also stream logs to stderr (serve does this by default; under the TUI
    /// it garbles the display, so redirect stderr when using it there).
    #[arg(long, global = true)]
    print_logs: bool,
    /// Auto-approve every permission prompt (recording the rules an "always"
    /// answer would persist).
    #[arg(long, global = true, visible_alias = "dangerously-skip-permissions")]
    yolo: bool,
    /// Open a specific thread. `--session` is an alias.
    #[arg(short = 's', long, global = true, visible_alias = "session")]
    thread: Option<Uuid>,
    /// Resume the last root session. A plain launch starts a fresh session.
    #[arg(
        short = 'c',
        long = "continue",
        global = true,
        conflicts_with = "thread"
    )]
    continue_last: bool,
}
#[derive(Subcommand)]
enum Command {
    /// Sign in to your Haarness account to use Cloud models and workspaces.
    Login(account::LoginArgs),
    /// Sign out of your Haarness account.
    Logout,
    /// Show or switch your Haarness account.
    Account(account::AccountArgs),
    /// Use Haarness Cloud services.
    Cloud(account::CloudArgs),
    /// Access this machine and selected chats through Haarness Cloud.
    Remote(remote_cloud::RemoteArgs),
    /// Private, versioned desktop bridge. Requests are read from standard input.
    #[command(hide = true)]
    Desktop,
    /// Manage trusted TypeScript/JavaScript runtime extensions.
    #[command(visible_alias = "extensions")]
    Extension(extensions::ExtensionArgs),
    /// Save, restore, and share portable configuration archives.
    Profile(profile::ProfileArgs),
    /// Cross-project token, cost, model and tool statistics from local history.
    Stats(insights::StatsArgs),
    /// Generate a private HTML report with cross-project findings.
    Insights(insights::InsightsArgs),
    /// Serve the Agent Client Protocol over stdin/stdout for editor integrations.
    Acp(AcpArgs),
    Serve(ServeArgs),
    /// Attach the terminal client to an already-running Haarness server.
    Attach(AttachArgs),
    Tui(TuiArgs),
    Run(RunArgs),
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// Browse and manage durable local sessions.
    #[command(alias = "session")]
    Sessions {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Ask an isolated, tool-free side question without changing a session transcript.
    Btw {
        id: Uuid,
        question: String,
        #[arg(long)]
        json: bool,
    },
    /// List project folders and create, open, reset, or remove Git worktrees.
    #[command(name = "worktree", aliases = ["workspace", "workspaces", "worktrees"])]
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
    /// List configured/discovered providers and connection state.
    Providers {
        #[arg(long)]
        verbose: bool,
    },
    /// Add, list, or remove custom provider definitions.
    Provider {
        #[command(subcommand)]
        command: ProviderCommand,
    },
    /// List built-in and configured agent profiles.
    Agents {
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Create, inspect, and remove local custom agents.
    #[command(name = "agent")]
    AgentManage {
        #[command(subcommand)]
        command: AgentManageCommand,
    },
    /// Create, inspect, and remove local slash commands.
    #[command(name = "command")]
    Prompt {
        #[command(subcommand)]
        command: CommandManageCommand,
    },
    /// Create, inspect, and remove local Agent Skills.
    #[command(name = "skill", visible_alias = "skills")]
    SkillManage {
        #[command(subcommand)]
        command: SkillManageCommand,
    },
    /// Browse, install and manage Haarness marketplace resources.
    Marketplace(marketplace::Args),
    /// Manage runtime capability availability as JSON.
    Capability {
        #[arg(default_value = "{\"action\":\"list\"}")]
        request: String,
    },
    /// Manage plugins (Claude Code marketplace format).
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },
    /// Discover, run, and manage durable dynamic workflows.
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommand,
    },
    /// Manage durable native scheduled runs.
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommand,
    },
    /// List every harness-compatible model, using live provider discovery
    /// when credentials are available.
    Models {
        /// Restrict output to one provider id.
        provider: Option<String>,
        /// Include context, output, capability, and pricing metadata as JSON.
        #[arg(long)]
        verbose: bool,
    },
    Doctor,
    /// Show where logs live, or tail the newest log file.
    Logs {
        /// Print the last N lines of the newest log file.
        #[arg(long)]
        tail: Option<usize>,
    },
    /// Install a checksum-verified release from Haarness Cloud.
    Upgrade(upgrade::UpgradeArgs),
}

#[derive(Args)]
struct AcpArgs {
    /// Override the provider used for newly-created ACP sessions.
    #[arg(long)]
    provider: Option<String>,
    /// Override the model used for newly-created ACP sessions.
    #[arg(short = 'm', long)]
    model: Option<String>,
    /// Override the agent profile used for newly-created ACP sessions.
    #[arg(long)]
    agent: Option<String>,
}

#[derive(Args, Clone, Default)]
struct PasswordArgs {
    /// Username for password authentication (defaults to haarness).
    #[arg(long, env = "HAARNESS_SERVER_USERNAME")]
    username: Option<String>,
    /// Optional password for server authentication.
    #[arg(long, env = "HAARNESS_SERVER_PASSWORD", hide_env_values = true)]
    password: Option<String>,
}

#[derive(Args)]
#[allow(clippy::struct_excessive_bools)] // independent CLI switches validated by clap
struct ServeArgs {
    #[command(flatten)]
    authentication: PasswordArgs,
    #[arg(long, default_value = "127.0.0.1")]
    host: IpAddr,
    #[arg(long, default_value_t = 4096)]
    port: u16,
    #[arg(long, env = "HAARNESS_SERVER_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// Start (or reuse) the workspace server in the background and return.
    #[arg(long, conflicts_with = "stop")]
    daemon: bool,
    /// Ask the discovered workspace server to stop gracefully.
    #[arg(long, conflicts_with = "daemon")]
    stop: bool,
    /// Internal marker used by the detached launcher.
    #[arg(long, hide = true)]
    background_child: bool,
    #[arg(long, hide = true)]
    anonymous: bool,
}
#[derive(Args)]
struct AttachArgs {
    #[command(flatten)]
    authentication: PasswordArgs,
    /// Base URL of the running Haarness server.
    url: String,
    /// Bearer token for the server. Defaults to HAARNESS_SERVER_TOKEN or a
    /// matching private local runtime discovery record.
    #[arg(long, env = "HAARNESS_SERVER_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// Fork the selected/continued session before opening it.
    #[arg(long)]
    fork: bool,
}
#[derive(Args)]
struct TuiArgs {
    #[command(flatten)]
    authentication: PasswordArgs,
    #[arg(long, visible_alias = "attach")]
    connect: Option<String>,
    #[arg(long, env = "HAARNESS_SERVER_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// Fork the selected/continued session before opening it.
    #[arg(long)]
    fork: bool,
}
#[derive(Args)]
// These booleans are independent, user-facing Clap switches rather than an
// application state machine; combining them into an enum would remove valid
// flag combinations and make the command-line contract less explicit.
#[allow(clippy::struct_excessive_bools)]
struct RunArgs {
    #[command(flatten)]
    authentication: PasswordArgs,
    /// Prompt text; when omitted, read it from piped stdin.
    #[arg(value_name = "MESSAGE")]
    prompt: Vec<String>,
    /// Attach a local PDF, image, audio, or video file (repeatable).
    /// Exact media support depends on the selected provider and model.
    #[arg(short = 'f', long = "file", value_name = "PATH")]
    files: Vec<PathBuf>,
    #[arg(long, default_value = "openai")]
    provider: String,
    /// Model id, or a qualified provider/model pair.
    #[arg(short = 'm', long, default_value = "gpt-5.6")]
    model: String,
    #[arg(long, default_value = "build")]
    agent: String,
    /// Opt this run into task-based workflow orchestration and the strongest
    /// reasoning effort supported by the selected model.
    #[arg(long)]
    ultracode: bool,
    /// Treat the positional or piped prompt as a standing objective and
    /// pursue it across turns until the goal reaches a terminal state.
    #[arg(long, conflicts_with_all = ["ultracode", "workflow"])]
    goal: bool,
    /// Reasoning/model variant for a normal turn or workflow-wide child-agent
    /// default. `--ultracode` is shorthand for the ultracode variant.
    #[arg(long, conflicts_with = "ultracode")]
    variant: Option<String>,
    /// Execute one discovered workflow directly instead of starting a model
    /// turn. The prompt becomes its `input` argument.
    #[arg(long, value_name = "NAME", conflicts_with = "ultracode")]
    workflow: Option<String>,
    /// Add a workflow argument as KEY=VALUE (repeatable). Values parse as JSON
    /// when possible and otherwise remain strings.
    #[arg(long = "workflow-arg", value_name = "KEY=VALUE", requires = "workflow")]
    workflow_args: Vec<String>,
    /// Expand and execute a discovered prompt command; positional text is
    /// passed as its arguments.
    #[arg(long, value_name = "NAME", conflicts_with_all = ["workflow", "goal", "ultracode"])]
    command: Option<String>,
    /// Title for a newly created CLI session.
    #[arg(long)]
    title: Option<String>,
    /// Output plain assistant text or newline-delimited JSON records.
    #[arg(long, value_enum, default_value_t = RunFormat::Default)]
    format: RunFormat,
    /// Wait for background subagents and workflows launched by the model.
    /// Without this flag, they continue on the workspace server.
    #[arg(long)]
    wait_background: bool,
    /// Maximum background wait in milliseconds; implies --wait-background.
    #[arg(long, value_name = "MS")]
    background_timeout_ms: Option<u64>,
    /// Include completed reasoning blocks in CLI output. This is the default;
    /// the flag remains accepted for script compatibility.
    #[arg(long)]
    thinking: bool,
    /// Hide completed reasoning blocks from CLI output.
    #[arg(long, conflicts_with = "thinking")]
    no_thinking: bool,
    /// Fork the selected/continued session before running. Requires
    /// `--session`/`--thread` or `--continue`.
    #[arg(long)]
    fork: bool,
    /// Run the chat on an already-running Haarness server instead of the
    /// auto-discovered local workspace server.
    #[arg(long, visible_alias = "connect", value_name = "URL")]
    attach: Option<String>,
    /// Bearer token for --attach. Defaults to HAARNESS_SERVER_TOKEN or a
    /// matching private local runtime discovery record.
    #[arg(long, env = "HAARNESS_SERVER_TOKEN", hide_env_values = true)]
    token: Option<String>,
}

impl RunArgs {
    fn show_thinking(&self) -> bool {
        self.thinking || !self.no_thinking
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum RunFormat {
    #[default]
    Default,
    Json,
}
#[derive(Subcommand)]
enum AuthCommand {
    /// Store an API key for a provider.
    Set {
        provider: String,
        #[arg(long)]
        key: Option<String>,
    },
    /// Add a provider credential (ChatGPT OAuth or an API key).
    Login {
        provider: String,
        /// OpenAI supports browser and headless ChatGPT login; xAI and GitHub
        /// Copilot support browser-assisted and headless device login. Other
        /// built-in providers use API keys.
        #[arg(long, value_enum)]
        method: Option<AuthLoginMethod>,
    },
    /// Log out from a provider and remove its stored credential.
    #[command(visible_alias = "remove")]
    Logout {
        provider: String,
    },
    List,
}

#[derive(Subcommand)]
enum ProviderCommand {
    /// Connect a self-hosted Haarness Router and discover its models.
    ConnectRouter {
        url: String,
        #[arg(long)]
        name: Option<String>,
        /// Read an optional router access key from stdin, never process arguments.
        #[arg(long)]
        key_stdin: bool,
        #[arg(long)]
        project: bool,
    },
    List {
        #[arg(long)]
        verbose: bool,
    },
    /// Add or replace a custom provider definition.
    Add {
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        url: String,
        /// Native wire transport (for example azure-openai, google-vertex,
        /// github-copilot, or amazon-bedrock).
        #[arg(long, default_value = "openai-chat-completions")]
        protocol: String,
        /// Environment variable containing the provider API key.
        #[arg(long)]
        api_key_env: Option<String>,
        /// Explicit model allowlist. Repeat for multiple models.
        #[arg(long = "model")]
        models: Vec<String>,
        /// Static request header as NAME=VALUE. Use NAME=${ENV_VAR} to keep
        /// the value out of the config file.
        #[arg(long = "header", value_name = "NAME=VALUE")]
        headers: Vec<String>,
        /// Mark the endpoint usable without a stored API key.
        #[arg(long)]
        no_auth: bool,
        /// Write to .haarness/config.toml instead of global config.
        #[arg(long, visible_alias = "local")]
        project: bool,
    },
    /// Remove a custom provider definition. Stored credentials are retained.
    Remove {
        id: String,
        #[arg(long, visible_alias = "local")]
        project: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum AuthLoginMethod {
    Browser,
    Headless,
    Api,
}
#[derive(Subcommand)]
enum McpCommand {
    #[command(alias = "ls")]
    List,
    Reconnect {
        name: String,
    },
    /// Close one live connection without changing its configuration.
    Disconnect {
        name: String,
    },
    /// List the tools currently advertised by one connected MCP server.
    Tools {
        name: String,
    },
    /// Invoke one advertised MCP tool with a JSON object.
    Call {
        name: String,
        tool: String,
        #[arg(long, default_value = "{}")]
        arguments: String,
    },
    /// List resources advertised by one connected MCP server.
    Resources {
        name: String,
    },
    /// List parameterized resource templates advertised by a server.
    ResourceTemplates {
        name: String,
    },
    /// Read one MCP resource by URI.
    Read {
        name: String,
        uri: String,
    },
    /// List prompts advertised by one connected MCP server.
    Prompts {
        name: String,
    },
    /// Resolve one MCP prompt with a JSON argument object.
    Prompt {
        name: String,
        prompt: String,
        #[arg(long, default_value = "{}")]
        arguments: String,
    },
    /// Authorize a remote MCP server using public OAuth 2.1 + PKCE.
    Auth {
        name: String,
        #[arg(long, visible_alias = "local")]
        project: bool,
        #[arg(long)]
        no_browser: bool,
        /// Loopback callback port; use 0 to request an ephemeral port.
        #[arg(long, default_value_t = 19_876)]
        callback_port: u16,
    },
    /// Remove the locally stored OAuth grant for one remote MCP server.
    Logout {
        name: String,
        #[arg(long, visible_alias = "local")]
        project: bool,
    },
    /// Show one MCP configuration. Environment and header values are redacted
    /// unless --show-secrets is explicitly supplied.
    Get {
        name: String,
        #[arg(long, visible_alias = "local")]
        project: bool,
        #[arg(long)]
        show_secrets: bool,
    },
    /// Add (or overwrite) an MCP server in the global config.
    /// Stdio: --command with optional --arg (repeatable) and --env KEY=VALUE.
    /// Remote: --url with optional --header KEY=VALUE (Streamable HTTP).
    Add {
        name: String,
        #[arg(long)]
        command: Option<String>,
        #[arg(long = "arg", allow_hyphen_values = true)]
        args: Vec<String>,
        #[arg(long = "env", value_name = "KEY=VALUE")]
        env: Vec<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long = "header", value_name = "KEY=VALUE")]
        headers: Vec<String>,
        /// Force generic MCP OAuth discovery for this remote server.
        #[arg(long, conflicts_with = "no_oauth")]
        oauth: bool,
        /// Disable generic OAuth auto-detection for this remote server.
        #[arg(long, conflicts_with = "oauth")]
        no_oauth: bool,
        /// Pre-registered public OAuth client id (otherwise use discovery/DCR).
        #[arg(long, requires = "url")]
        oauth_client_id: Option<String>,
        /// OAuth scope to request. Repeat for multiple scopes.
        #[arg(long = "oauth-scope", requires = "url")]
        oauth_scopes: Vec<String>,
        /// Add the entry without connecting it.
        #[arg(long)]
        disabled: bool,
        /// Write to the project's .haarness/config.toml instead of global.
        #[arg(long, visible_alias = "local")]
        project: bool,
    },
    /// Enable one configured MCP server.
    Enable {
        name: String,
        #[arg(long, visible_alias = "local")]
        project: bool,
    },
    /// Disable one configured MCP server without deleting its configuration.
    Disable {
        name: String,
        #[arg(long, visible_alias = "local")]
        project: bool,
    },
    /// Remove an MCP server from the global (or --project) config.
    #[command(alias = "rm")]
    Remove {
        name: String,
        #[arg(long, visible_alias = "local")]
        project: bool,
    },
}

#[derive(Subcommand)]
enum AgentManageCommand {
    #[command(alias = "ls")]
    List {
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    Get {
        name: String,
    },
    Create {
        name: String,
        #[arg(long)]
        description: String,
        #[arg(long, conflicts_with = "file")]
        prompt: Option<String>,
        #[arg(long, value_name = "PATH", conflicts_with = "prompt")]
        file: Option<PathBuf>,
        #[arg(long)]
        mode: Option<String>,
        #[arg(long)]
        model: Option<String>,
        /// Per-agent tool ceiling, repeatable (for example: --tool bash=false).
        #[arg(long = "tool", value_name = "NAME=BOOL")]
        tools: Vec<String>,
        /// Per-agent permission rule, repeatable (for example: --permission 'edit:*=deny').
        #[arg(long = "permission", value_name = "PATTERN=allow|ask|deny")]
        permissions: Vec<String>,
        #[arg(long, visible_alias = "local")]
        project: bool,
        #[arg(long)]
        force: bool,
    },
    #[command(alias = "rm", alias = "delete")]
    Remove {
        name: String,
        #[arg(long, visible_alias = "local")]
        project: bool,
    },
}

#[derive(Subcommand)]
enum CommandManageCommand {
    #[command(alias = "ls")]
    List,
    Get {
        name: String,
    },
    Create {
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long, conflicts_with = "file")]
        template: Option<String>,
        #[arg(long, value_name = "PATH", conflicts_with = "template")]
        file: Option<PathBuf>,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        argument_hint: Option<String>,
        #[arg(long, visible_alias = "local")]
        project: bool,
        #[arg(long)]
        force: bool,
    },
    #[command(alias = "rm", alias = "delete")]
    Remove {
        name: String,
        #[arg(long, visible_alias = "local")]
        project: bool,
    },
}

#[derive(Subcommand)]
enum SkillManageCommand {
    #[command(alias = "ls")]
    List,
    Get {
        name: String,
    },
    Create {
        name: String,
        #[arg(long)]
        description: String,
        #[arg(long, conflicts_with = "file")]
        instructions: Option<String>,
        #[arg(long, value_name = "PATH", conflicts_with = "instructions")]
        file: Option<PathBuf>,
        #[arg(long, visible_alias = "local")]
        project: bool,
        #[arg(long)]
        force: bool,
    },
    #[command(alias = "rm", alias = "delete")]
    Remove {
        name: String,
        #[arg(long, visible_alias = "local")]
        project: bool,
    },
}

#[derive(Subcommand)]
enum SessionCommand {
    #[command(alias = "ls")]
    List {
        /// Include archived sessions.
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    Get {
        id: Uuid,
        /// Include the complete transcript item stream.
        #[arg(long)]
        transcript: bool,
    },
    Archive {
        id: Uuid,
    },
    Unarchive {
        id: Uuid,
    },
    Fork {
        id: Uuid,
        #[arg(long)]
        title: Option<String>,
        /// Fork only through this transcript sequence.
        #[arg(long)]
        at: Option<u64>,
    },
    /// Export a session and its complete descendant tree as portable JSON.
    Export {
        id: Uuid,
        /// Replace transcript content and other potentially sensitive text
        /// with stable redaction markers.
        #[arg(long)]
        sanitize: bool,
        /// Write the archive to a file instead of standard output.
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,
        /// Replace an existing output file.
        #[arg(long, requires = "output")]
        force: bool,
    },
    /// Restore or idempotently merge a portable session archive.
    Import {
        #[arg(value_name = "FILE")]
        file: PathBuf,
        /// Print the complete machine-readable import result.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum WorkspaceCommand {
    /// List registered projects, their main folders, and worktrees.
    #[command(alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
    },
    /// Create a linked Git worktree with a unique branch.
    Create {
        name: String,
        #[arg(long = "from")]
        from_ref: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Open a project folder or worktree in the TUI.
    Open {
        reference: String,
        /// Resolve and print the path without launching a TUI.
        #[arg(long)]
        path_only: bool,
    },
    /// Move a managed worktree branch back to its recorded (or supplied) ref.
    Reset {
        reference: String,
        /// Required acknowledgement that commits may be discarded.
        #[arg(long)]
        confirm: bool,
        /// Also discard tracked working-tree modifications.
        #[arg(long)]
        discard_changes: bool,
        /// Also delete untracked files. Ignored files are always preserved.
        #[arg(long, requires = "discard_changes")]
        remove_untracked: bool,
        #[arg(long = "to")]
        to_ref: Option<String>,
    },
    /// Remove a Haarness-managed worktree. Local workspaces are never removed.
    #[command(alias = "rm")]
    Remove {
        reference: String,
        /// Required acknowledgement that the checkout directory is deleted.
        #[arg(long)]
        confirm: bool,
        /// Permit removal when the worktree has local changes.
        #[arg(long)]
        discard_changes: bool,
        /// Delete the branch only when Git confirms it is merged.
        #[arg(long)]
        delete_branch: bool,
    },
}

#[derive(Subcommand)]
enum PluginCommand {
    /// Register a marketplace: a git URL, owner/repo, or local directory
    /// holding .claude-plugin/marketplace.json.
    Marketplace {
        source: String,
        /// Compatibility positional for `plugin marketplace add <source>`.
        compatibility_source: Option<String>,
    },
    /// Install a plugin: name@marketplace, or a local plugin directory/plugin.json.
    Install {
        spec: String,
    },
    /// Refresh an installed marketplace plugin from its source.
    Update {
        name: String,
    },
    /// Installed plugins and registered marketplaces.
    List,
    Enable {
        name: String,
    },
    Disable {
        name: String,
    },
    Remove {
        name: String,
    },
}

#[derive(Subcommand)]
enum WorkflowCommand {
    /// List reusable project, plugin, and global workflow definitions.
    List,
    /// Print one discovered workflow's source.
    Source {
        name: String,
    },
    /// List recent durable runs.
    Runs {
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Start a named workflow. By default this waits for completion.
    Run {
        name: String,
        /// JSON value passed to the workflow.
        #[arg(long)]
        args: Option<String>,
        /// Workflow-wide child-agent profile default.
        #[arg(long)]
        agent: Option<String>,
        /// Workflow-wide child-agent model default as provider/model.
        #[arg(long)]
        model: Option<String>,
        /// Workflow-wide child-agent reasoning/model variant default.
        #[arg(long)]
        variant: Option<String>,
        /// Return immediately after the durable run is created.
        #[arg(long)]
        detach: bool,
        #[arg(long)]
        timeout_ms: Option<u64>,
    },
    Inspect {
        run_id: Uuid,
    },
    Cancel {
        run_id: Uuid,
    },
    Pause {
        run_id: Uuid,
    },
    Resume {
        run_id: Uuid,
    },
    Delete {
        run_id: Uuid,
    },
}

#[derive(Subcommand)]
enum ScheduleCommand {
    #[command(alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
    },
    Get {
        reference: String,
    },
    Add(ScheduleAddArgs),
    Update(ScheduleUpdateArgs),
    #[command(alias = "rm")]
    Remove {
        reference: String,
    },
    Enable {
        reference: String,
    },
    Disable {
        reference: String,
    },
    Run {
        reference: String,
    },
    History {
        reference: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    Logs {
        reference: Option<String>,
    },
    /// Internal native-service entry point.
    #[command(hide = true)]
    Trigger {
        reference: String,
        #[arg(long)]
        source: ScheduleRunSource,
    },
}

#[derive(Args)]
struct ScheduleAddArgs {
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    prompt: Option<String>,
    #[arg(long)]
    natural: Option<String>,
    #[arg(long)]
    directory: Option<PathBuf>,
    /// Provider-qualified model, for example openai/gpt-5.6.
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    agent: Option<String>,
    #[arg(long)]
    variant: Option<String>,
    #[arg(long)]
    timezone: Option<String>,
    #[arg(long)]
    disabled: bool,
    #[arg(long, conflicts_with_all = ["cron", "at"])]
    every: Option<String>,
    #[arg(long, conflicts_with_all = ["every", "at"])]
    cron: Option<String>,
    #[arg(long, conflicts_with_all = ["every", "cron"])]
    at: Option<String>,
}

#[derive(Args)]
struct ScheduleUpdateArgs {
    reference: String,
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    prompt: Option<String>,
    #[arg(long)]
    natural: Option<String>,
    #[arg(long)]
    directory: Option<PathBuf>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    agent: Option<String>,
    #[arg(long)]
    variant: Option<String>,
    #[arg(long)]
    timezone: Option<String>,
    #[arg(long, conflicts_with = "disable")]
    enable: bool,
    #[arg(long, conflicts_with = "enable")]
    disable: bool,
    #[arg(long, conflicts_with_all = ["cron", "at"])]
    every: Option<String>,
    #[arg(long, conflicts_with_all = ["every", "at"])]
    cron: Option<String>,
    #[arg(long, conflicts_with_all = ["every", "cron"])]
    at: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse_from(normalize_natural_language_args(std::env::args_os()));
    let project_config = !cli.disable_project_config;
    let project_config_override = if cli.disable_project_config {
        Some(false)
    } else {
        cli.trust_project.then_some(true)
    };
    let base_paths = Paths::new(cli.data_dir.as_deref())?;
    // Analytics must not register workspaces, migrate databases, start daemons or load credentials.
    match &cli.command {
        Some(Command::Login(args)) => return account::login(args).await,
        Some(Command::Logout) => return account::logout().await,
        Some(Command::Account(args)) => return account::account(args).await,
        Some(Command::Cloud(args)) => return account::cloud(args).await,
        Some(Command::Profile(args)) => {
            return profile::run(&base_paths, &cli.workspace, args).await;
        }
        Some(Command::Stats(args)) => {
            return insights::stats(&base_paths.data, &cli.workspace, args);
        }
        Some(Command::Insights(args)) => {
            return insights::insights(&base_paths.data, &cli.workspace, args);
        }
        _ => {}
    }
    // Logs always land in <data>/logs (rotated daily) so problems seen in
    // the TUI can be run down afterwards; stderr is opt-in because the TUI
    // owns the terminal. `serve` streams to stderr by default.
    let stderr_logs = cli.print_logs || matches!(cli.command, Some(Command::Serve(_)));
    // Read-only commands stay out of the log files they inspect.
    let file_logs = !matches!(cli.command, Some(Command::Logs { .. } | Command::Doctor));
    let telemetry_guard = init_logging(
        &base_paths,
        cli.log_level.as_deref(),
        stderr_logs,
        file_logs,
    )?;
    let command_name = cli.command.as_ref().map_or("tui", Command::name);
    let workspace = cli
        .workspace
        .canonicalize()
        .context("canonicalize workspace")?;
    let workspace_manager = base_paths.workspace_manager()?;
    let current_workspace = workspace_manager
        .register_local(&workspace)
        .map_err(anyhow::Error::new)?;
    let migrate_legacy = workspace_manager.registered_count()? == 1;
    let paths = base_paths.scoped(&current_workspace.id, migrate_legacy)?;
    let command_span = tracing::info_span!(
        "haarness.command",
        "haarness.command.name" = command_name,
        "workspace.id" = %current_workspace.id,
    );
    let outcome = async {
        tracing::info!(
            version = env!("CARGO_PKG_VERSION"),
            workspace = %workspace.display(),
            "haarness starting"
        );
        match cli.command {
        Some(Command::Remote(args)) => remote_cloud::run(&paths, &workspace, project_config, args).await,
        Some(Command::Desktop) => desktop::run(&paths, &workspace, project_config).await,
        Some(Command::Login(_) | Command::Logout | Command::Account(_) | Command::Cloud(_)) => unreachable!("account command handled before project setup"),
        Some(Command::Stats(_) | Command::Insights(_) | Command::Profile(_)) => unreachable!("standalone commands dispatched before runtime setup"),
        None => {
            interactive(
                &paths,
                workspace,
                project_config,
                cli.yolo,
                cli.thread,
                cli.continue_last,
            )
            .await
        }
        Some(Command::Acp(args)) => {
            let discovery =
                ensure_workspace_server(&paths, &workspace, project_config, cli.yolo).await?;
            acp::serve(acp::Config {
                client: haarness_tui::api::ApiClient::new(&discovery.url, &discovery.token)?,
                workspace,
                provider: args.provider,
                model: args.model,
                agent: args.agent,
                auto_approve: cli.yolo,
            })
            .await
        }
        Some(Command::Serve(args)) => {
            serve_command(&paths, workspace, project_config, cli.yolo, args).await
        }
        Some(Command::Attach(args)) => {
            attach_command(&paths, &workspace, args, cli.thread, cli.continue_last).await
        }
        Some(Command::Tui(args)) => {
            tui_command(
                &paths,
                &workspace,
                project_config,
                cli.yolo,
                args,
                cli.thread,
                cli.continue_last,
            )
            .await
        }
        Some(Command::Run(args)) => {
            run_command(
                &paths,
                workspace,
                project_config,
                cli.yolo,
                cli.thread,
                cli.continue_last,
                args,
            )
            .await
        }
        Some(Command::Auth { command }) => auth_command(&paths, command).await,
        Some(Command::Mcp { command }) => {
            mcp_command(&paths, &workspace, project_config, cli.yolo, command).await
        }
        Some(Command::Sessions { command }) => {
            session_command(&paths, &workspace, project_config, cli.yolo, command).await
        }
        Some(Command::Btw { id, question, json }) => {
            btw_command(
                &paths,
                &workspace,
                project_config,
                cli.yolo,
                ThreadId(id),
                question,
                json,
            )
            .await
        }
        Some(Command::Workspace { command }) => {
            workspace_command(
                &paths,
                &current_workspace.id,
                project_config,
                cli.yolo,
                cli.thread,
                cli.continue_last,
                command,
            )
            .await
        }
        Some(Command::Providers { verbose }) => {
            providers_command(&paths, &workspace, project_config, verbose).await
        }
        Some(Command::Provider { command }) => {
            provider_command(&paths, &workspace, project_config, command).await
        }
        Some(Command::Agents { all, json }) => {
            agents_command(&paths, &workspace, project_config, all, json)
        }
        Some(Command::AgentManage { command }) => {
            agent_manage_command(&paths, &workspace, project_config, command)
        }
        Some(Command::Prompt { command }) => {
            command_manage_command(&paths, &workspace, project_config, command)
        }
        Some(Command::SkillManage { command }) => {
            skill_manage_command(&paths, &workspace, project_config, command).await
        }
        Some(Command::Extension(args)) => extensions::run(&paths, &workspace, project_config, args).await,
        Some(Command::Marketplace(args)) => marketplace::run(&paths, &workspace, project_config, args).await,
        Some(Command::Capability { request }) => {
            let service=marketplace::service(&paths,&workspace,project_config).await?;
            println!("{}",serde_json::to_string_pretty(&service.capability_request(&serde_json::from_str(&request)?).await?)?);
            Ok(())
        },
        Some(Command::Plugin { command }) => {
            plugin_command(&paths, &workspace, project_config, &command)
        }
        Some(Command::Workflow { command }) => {
            workflow_command(&paths, &workspace, project_config, cli.yolo, command).await
        }
        Some(Command::Schedule { command }) => {
            schedule_command(&paths, &workspace, project_config, cli.yolo, project_config_override, command).await
        }
        Some(Command::Models { provider, verbose }) => {
            models_command(
                &paths,
                &workspace,
                project_config,
                provider.as_deref(),
                verbose,
            )
            .await
        }
        Some(Command::Doctor) => doctor(&paths),
        Some(Command::Logs { tail }) => logs_command(&paths, tail),
        Some(Command::Upgrade(args)) => upgrade::run(args),
        }
    }
    .instrument(command_span)
    .await;
    drop(telemetry_guard);
    outcome
}

impl Command {
    const fn name(&self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Login(_) => "login",
            Self::Logout => "logout",
            Self::Account(_) => "account",
            Self::Cloud(_) => "cloud",
            Self::Remote(_) => "remote",
            Self::Profile(_) => "profile",
            Self::Extension(_) => "extension",
            Self::Stats(_) => "stats",
            Self::Insights(_) => "insights",
            Self::Acp(_) => "acp",
            Self::Serve(_) => "serve",
            Self::Attach(_) => "attach",
            Self::Tui(_) => "tui",
            Self::Run(_) => "run",
            Self::Auth { .. } => "auth",
            Self::Mcp { .. } => "mcp",
            Self::Sessions { .. } => "sessions",
            Self::Btw { .. } => "btw",
            Self::Workspace { .. } => "workspace",
            Self::Providers { .. } => "providers",
            Self::Provider { .. } => "provider",
            Self::Agents { .. } => "agents",
            Self::AgentManage { .. } => "agent",
            Self::Prompt { .. } => "command",
            Self::SkillManage { .. } => "skill",
            Self::Marketplace(..) => "marketplace",
            Self::Capability { .. } => "capability",
            Self::Plugin { .. } => "plugin",
            Self::Workflow { .. } => "workflow",
            Self::Schedule { .. } => "schedule",
            Self::Models { .. } => "models",
            Self::Doctor => "doctor",
            Self::Logs { .. } => "logs",
            Self::Upgrade(_) => "upgrade",
        }
    }
}

fn normalize_natural_language_args(args: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    const COMMANDS: &[&str] = &[
        "login",
        "logout",
        "account",
        "cloud",
        "profile",
        "stats",
        "insights",
        "acp",
        "serve",
        "attach",
        "tui",
        "run",
        "auth",
        "mcp",
        "sessions",
        "session",
        "btw",
        "worktree",
        "worktrees",
        "workspace",
        "workspaces",
        "providers",
        "provider",
        "agents",
        "agent",
        "command",
        "skill",
        "skills",
        "plugin",
        "workflow",
        "schedule",
        "models",
        "doctor",
        "logs",
        "upgrade",
        "help",
    ];
    let mut args = args.into_iter().collect::<Vec<_>>();
    let Some(first) = args.get(1).and_then(|value| value.to_str()) else {
        return args;
    };
    if first.starts_with('-')
        || COMMANDS.contains(&first)
        || pathlike_cli_argument(first)
        || Path::new(first).exists()
    {
        return args;
    }
    let option_index = args
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, value)| {
            let value = value.to_string_lossy();
            (value == "--" || value.starts_with('-')).then_some(index)
        })
        .unwrap_or(args.len());
    if option_index == 1 {
        return args;
    }
    args.insert(1, OsString::from("run"));
    args
}

fn pathlike_cli_argument(value: &str) -> bool {
    value == "."
        || value == ".."
        || value.starts_with("./")
        || value.starts_with("../")
        || value.starts_with(".\\")
        || value.starts_with("..\\")
        || value.starts_with("~/")
        || value.starts_with("~\\")
        || value.starts_with('/')
        || value.starts_with('\\')
        || value.contains('/')
        || value.contains('\\')
        || value
            .as_bytes()
            .get(1)
            .is_some_and(|character| *character == b':')
}

async fn models_command(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    provider_filter: Option<&str>,
    verbose: bool,
) -> Result<()> {
    let config = load_config(paths, workspace, trust)?;
    let credentials = resolved_credentials(&read_auth(paths).unwrap_or_default());
    let (instances, disabled) = provider_instances(&config, &credentials);
    let providers = NativeProviderRegistry::discover_with_providers_and_store(
        &credentials,
        instances,
        &disabled,
        Some(Arc::new(CredentialStore::new(paths.auth.clone()))),
    )
    .await;
    let mut found_provider = provider_filter.is_none();
    for provider in providers.list().into_iter().filter(|provider| {
        provider_filter
            .is_none_or(|filter| provider.id == haarness_protocol::provider_storage_id(filter))
    }) {
        found_provider = true;
        for model in provider.models {
            println!(
                "{}/{}",
                haarness_protocol::provider_display_id(&provider.id),
                model.id
            );
            if verbose {
                println!("{}", serde_json::to_string_pretty(&model)?);
            }
        }
    }
    if !found_provider {
        anyhow::bail!(
            "provider not found: {}",
            provider_filter.unwrap_or_default()
        );
    }
    Ok(())
}

async fn providers_command(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    verbose: bool,
) -> Result<()> {
    let config = load_config(paths, workspace, trust)?;
    let credentials = resolved_credentials(&read_auth(paths).unwrap_or_default());
    let (instances, disabled) = provider_instances(&config, &credentials);
    let providers = NativeProviderRegistry::discover_with_providers_and_store(
        &credentials,
        instances,
        &disabled,
        Some(Arc::new(CredentialStore::new(paths.auth.clone()))),
    )
    .await;
    for provider in providers.list() {
        if verbose {
            println!("{}", serde_json::to_string_pretty(&provider)?);
        } else {
            println!(
                "{}\t{}\t{} models\t{}",
                provider.id,
                provider.name,
                provider.models.len(),
                if provider.authenticated {
                    "connected"
                } else {
                    "not authenticated"
                }
            );
        }
    }
    Ok(())
}

async fn provider_command(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    command: ProviderCommand,
) -> Result<()> {
    match command {
        ProviderCommand::ConnectRouter {
            url,
            name,
            key_stdin,
            project,
        } => {
            let mut key = String::new();
            if key_stdin {
                std::io::stdin().read_line(&mut key)?;
            }
            let result = router_connection::connect(
                paths,
                workspace,
                trust,
                &router_connection::ConnectRequest {
                    url,
                    name: name.unwrap_or_default(),
                    key: key.trim().into(),
                    project,
                },
            )
            .await?;
            match result {
                router_connection::ConnectResult::NeedsKey => anyhow::bail!(
                    "This router requires an access key. Supply it through --key-stdin."
                ),
                router_connection::ConnectResult::Connected { id, models, .. } => {
                    println!("Connected {id} · {models} models available")
                }
            }
            Ok(())
        }
        ProviderCommand::List { verbose } => {
            providers_command(paths, workspace, trust, verbose).await
        }
        ProviderCommand::Add {
            id,
            name,
            url,
            protocol,
            api_key_env,
            models,
            headers,
            no_auth,
            project,
        } => {
            validate_provider_id(&id)?;
            haarness_providers::CompatibleProtocol::parse(&protocol).map_err(anyhow::Error::msg)?;
            if !valid_provider_url(&url) {
                anyhow::bail!(
                    "provider URL must use https, or http on a loopback address for a local runtime"
                );
            }
            let headers = parse_mcp_key_values(&headers, "header")?;
            // Validate names, values, and environment references before the
            // config write. Secret values are never included in errors.
            provider_headers(&headers).map_err(anyhow::Error::msg)?;
            if models.iter().any(|model| model.trim().is_empty()) {
                anyhow::bail!("provider model ids must be non-empty");
            }
            let section = ProviderSection {
                protocol,
                name: Some(name.unwrap_or_else(|| id.clone())),
                base_url: Some(url),
                api_key_env,
                headers,
                models,
                requires_auth: !no_auth,
                ..ProviderSection::default()
            };
            let target = provider_config_target(paths, workspace, project);
            write_provider_entry(&target, &id, Some(&section))?;
            println!("Added custom provider '{id}' to {}.", target.display());
            if section.requires_auth && section.api_key_env.is_none() && section.headers.is_empty()
            {
                println!("Connect it with `haarness auth set {id}` before use.");
            }
            if project && !trust {
                println!("Note: project configuration is disabled for this invocation.");
            }
            println!("Restart the workspace server to load provider definition changes.");
            Ok(())
        }
        ProviderCommand::Remove { id, project } => {
            validate_provider_id(&id)?;
            let target = provider_config_target(paths, workspace, project);
            write_provider_entry(&target, &id, None)?;
            println!("Removed custom provider '{id}' from {}.", target.display());
            println!(
                "Stored credentials were retained; use `haarness auth logout {id}` to remove them."
            );
            println!("Restart the workspace server to unload the provider definition.");
            Ok(())
        }
    }
}

fn validate_provider_id(id: &str) -> Result<()> {
    if id.is_empty()
        || !id.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '-' | '_')
        })
    {
        anyhow::bail!(
            "provider id must contain only lowercase ASCII letters, digits, hyphens, or underscores"
        );
    }
    Ok(())
}

fn valid_provider_url(value: &str) -> bool {
    let Some((scheme, rest)) = value.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return false;
    }
    if scheme == "https" {
        return true;
    }
    if scheme != "http" {
        return false;
    }
    authority == "localhost"
        || authority.starts_with("localhost:")
        || authority == "127.0.0.1"
        || authority.starts_with("127.0.0.1:")
        || authority == "[::1]"
        || authority.starts_with("[::1]:")
}

fn agents_command(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    include_hidden: bool,
    json_output: bool,
) -> Result<()> {
    let config_dir = paths
        .config
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let plugin_roots =
        plugins::all_enabled_roots(&paths.data, paths.config_dir(), workspace, trust);
    let layers = jsonconfig::load_layers(
        &config_dir,
        home.as_deref(),
        workspace,
        trust,
        &plugin_roots,
    );
    let mut profiles = haarness_core::builtin_profiles();
    for agent in layers.agents {
        if let Some(profile) = profiles.iter_mut().find(|profile| profile.id == agent.id) {
            profile.description = agent.description;
            profile.mode = agent.mode;
            profile.model = agent.model;
        } else {
            profiles.push(haarness_protocol::AgentProfile {
                id: agent.id.clone(),
                name: agent.id,
                description: agent.description,
                hidden: agent.hidden,
                max_subagents: 4,
                mode: agent.mode,
                model: agent.model,
            });
        }
    }
    let config = load_config(paths, workspace, trust)?;
    let execution: haarness_core::ExecutionSettings =
        serde_json::from_value(serde_json::to_value(&config.execution)?)?;
    execution.validate()?;
    for profile in &mut profiles {
        profile.max_subagents = if execution.subagent_depth == 0 || profile.max_subagents == 0 {
            0
        } else {
            u8::try_from(execution.subagent_concurrency).unwrap_or(u8::MAX)
        };
    }
    profiles.retain(|profile| include_hidden || !profile.hidden);
    if json_output {
        println!("{}", serde_json::to_string_pretty(&profiles)?);
        return Ok(());
    }
    for profile in profiles {
        let mut details = Vec::new();
        if let Some(mode) = profile.mode {
            details.push(format!("mode={mode}"));
        }
        if let Some(model) = profile.model {
            details.push(format!("model={model}"));
        }
        let suffix = if details.is_empty() {
            String::new()
        } else {
            format!("\t{}", details.join(" "))
        };
        println!("{}\t{}{}", profile.id, profile.description, suffix);
    }
    Ok(())
}

fn asset_layers(paths: &Paths, workspace: &Path, trust: bool) -> jsonconfig::JsonLayers {
    let config = paths
        .config
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let plugins = plugins::all_enabled_roots(&paths.data, paths.config_dir(), workspace, trust);
    jsonconfig::load_layers(&config, home.as_deref(), workspace, trust, &plugins)
}

fn agent_manage_command(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    command: AgentManageCommand,
) -> Result<()> {
    match &command {
        AgentManageCommand::List { .. } => {}
        AgentManageCommand::Get { name }
        | AgentManageCommand::Create { name, .. }
        | AgentManageCommand::Remove { name, .. } => management::ensure_user_agent(name)?,
    }
    match command {
        AgentManageCommand::List { all, json } => {
            agents_command(paths, workspace, trust, all, json)
        }
        AgentManageCommand::Get { name } => {
            let agent = asset_layers(paths, workspace, trust)
                .agents
                .into_iter()
                .find(|agent| agent.id == name)
                .with_context(|| format!("no custom agent named '{name}'"))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "name": agent.id,
                    "description": agent.description,
                    "mode": agent.mode,
                    "model": agent.model,
                    "tools": agent.tools,
                    "permissions": agent.permissions,
                    "prompt": agent.prompt,
                }))?
            );
            Ok(())
        }
        AgentManageCommand::Create {
            name,
            description,
            prompt,
            file,
            mode,
            model,
            tools,
            permissions,
            project,
            force,
        } => {
            validate_asset_name(&name, "agent")?;
            if let Some(mode) = mode.as_deref()
                && !matches!(mode, "primary" | "subagent" | "all")
            {
                anyhow::bail!("agent mode must be primary, subagent, or all");
            }
            if let Some(model) = model.as_deref() {
                validate_qualified_model(model, "agent")?;
            }
            let tools = parse_agent_tools(&tools)?;
            let permissions = parse_agent_permissions(&permissions)?;
            let tools_json = if tools.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&tools)?)
            };
            let permissions_json = if permissions.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&permissions)?)
            };
            let body = asset_body(prompt, file, "agent prompt")?;
            let target = managed_asset_path(paths, workspace, project, "agents", &name, "md")?;
            let mut metadata = vec![
                ("name", Some(name.as_str())),
                ("description", Some(description.as_str())),
                ("mode", mode.as_deref()),
                ("model", model.as_deref()),
                ("tools", tools_json.as_deref()),
                ("permissions", permissions_json.as_deref()),
            ];
            write_markdown_asset(&target, &mut metadata, &body, force)?;
            println!("Created agent '{name}' at {}.", target.display());
            reload_notice(paths);
            Ok(())
        }
        AgentManageCommand::Remove { name, project } => {
            let target = managed_asset_path(paths, workspace, project, "agents", &name, "md")?;
            remove_managed_asset(&target, false)?;
            println!("Removed agent '{name}' from {}.", target.display());
            reload_notice(paths);
            Ok(())
        }
    }
}

fn command_manage_command(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    command: CommandManageCommand,
) -> Result<()> {
    match command {
        CommandManageCommand::List => {
            for command in asset_layers(paths, workspace, trust).commands {
                println!("{}\t{}", command.name, command.description);
            }
            Ok(())
        }
        CommandManageCommand::Get { name } => {
            let command = asset_layers(paths, workspace, trust)
                .commands
                .into_iter()
                .find(|command| command.name.eq_ignore_ascii_case(&name))
                .with_context(|| format!("no command named '{name}'"))?;
            println!("{}", serde_json::to_string_pretty(&command)?);
            Ok(())
        }
        CommandManageCommand::Create {
            name,
            description,
            template,
            file,
            agent,
            model,
            argument_hint,
            project,
            force,
        } => {
            validate_asset_name(&name, "command")?;
            if let Some(model) = model.as_deref() {
                validate_qualified_model(model, "command")?;
            }
            let body = asset_body(template, file, "command template")?;
            let target = managed_asset_path(paths, workspace, project, "commands", &name, "md")?;
            let mut metadata = vec![
                ("description", Some(description.as_str())),
                ("agent", agent.as_deref()),
                ("model", model.as_deref()),
                ("argument-hint", argument_hint.as_deref()),
            ];
            write_markdown_asset(&target, &mut metadata, &body, force)?;
            println!("Created command '/{name}' at {}.", target.display());
            reload_notice(paths);
            Ok(())
        }
        CommandManageCommand::Remove { name, project } => {
            let target = managed_asset_path(paths, workspace, project, "commands", &name, "md")?;
            remove_managed_asset(&target, false)?;
            println!("Removed command '/{name}' from {}.", target.display());
            reload_notice(paths);
            Ok(())
        }
    }
}

async fn skill_manage_command(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    command: SkillManageCommand,
) -> Result<()> {
    let file_config = load_config(paths, workspace, trust)?;
    let registry =
        FilesystemSkillRegistry::new_scoped(skill_roots(paths, workspace, trust, &file_config));
    match command {
        SkillManageCommand::List => {
            for skill in registry.discover().await? {
                println!("{}\t{}\t{}", skill.name, skill.description, skill.path);
            }
            Ok(())
        }
        SkillManageCommand::Get { name } => {
            println!("{}", registry.load(&name).await?);
            Ok(())
        }
        SkillManageCommand::Create {
            name,
            description,
            instructions,
            file,
            project,
            force,
        } => {
            validate_asset_name(&name, "skill")?;
            let body = asset_body(instructions, file, "skill instructions")?;
            let root = if project {
                workspace.join(".haarness/skills")
            } else {
                paths.skills.clone()
            };
            let directory = managed_name_path(&root, &name)?;
            let target = directory.join("SKILL.md");
            let mut metadata = vec![
                ("name", Some(name.as_str())),
                ("description", Some(description.as_str())),
            ];
            write_markdown_asset(&target, &mut metadata, &body, force)?;
            println!("Created skill '{name}' at {}.", target.display());
            reload_notice(paths);
            Ok(())
        }
        SkillManageCommand::Remove { name, project } => {
            let root = if project {
                workspace.join(".haarness/skills")
            } else {
                paths.skills.clone()
            };
            let target = managed_name_path(&root, &name)?;
            remove_managed_asset(&target, true)?;
            println!("Removed skill '{name}' from {}.", target.display());
            reload_notice(paths);
            Ok(())
        }
    }
}

/// Ordered skill roots: plugins, Agent Skills homes, Claude Code roots when
/// `compatibility.claude_code.skills` opts in, native global/project roots,
/// then `skills.paths` folders and cached `skills.urls` indexes. Later roots
/// win on name collisions.
fn skill_roots(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    file_config: &FileConfig,
) -> Vec<(PathBuf, bool, Option<PathBuf>)> {
    let config = paths
        .config
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let plugins = plugins::all_enabled_roots(&paths.data, paths.config_dir(), workspace, trust);
    let mut roots = plugins
        .into_iter()
        .flat_map(|root| {
            haarness_core::definitions::component_paths(&root, "skills")
                .into_iter()
                .map(move |path| (path, true, Some(root.clone())))
        })
        .collect::<Vec<_>>();
    if let Some(home) = home.as_deref() {
        roots.push((home.join(".agents/skills"), true, None));
    }
    if claude_compat(file_config).skills() {
        if let Some(home) = home.as_deref() {
            roots.push((home.join(".claude/skills"), true, None));
        }
        if trust {
            for directory in layout::project_directories(workspace) {
                roots.push((directory.join(".claude/skills"), true, None));
            }
        }
    }
    roots.push((paths.skills.clone(), true, None));
    for root in layout::resource_roots(&config, home.as_deref(), workspace, trust) {
        for name in ["skill", "skills"] {
            roots.push((root.join(name), true, None));
        }
    }
    if trust {
        for directory in layout::project_directories(workspace) {
            roots.push((directory.join(".agents/skills"), true, None));
        }
    }
    for path in jsonconfig::configured_skill_paths(file_config, home.as_deref(), workspace) {
        roots.push((path, true, None));
    }
    for root in remote_skill_roots(paths, file_config) {
        roots.push((root, true, None));
    }
    roots
}

/// Cache directory for skills pulled from `skills.urls`.
fn remote_skill_cache(paths: &Paths) -> PathBuf {
    paths.data.join("remote-skills")
}

/// Pull every configured remote skill index (best effort) and return the
/// cache roots that now hold at least one skill.
fn remote_skill_roots(paths: &Paths, file_config: &FileConfig) -> Vec<PathBuf> {
    let cache = remote_skill_cache(paths);
    let mut roots = Vec::new();
    for url in file_config
        .skills
        .as_ref()
        .map(|skills| skills.urls.as_slice())
        .unwrap_or_default()
    {
        match jsonconfig::pull_remote_skills(&cache, url) {
            Ok(Some(root)) => {
                if !roots.contains(&root) {
                    roots.push(root);
                }
            }
            Ok(None) => tracing::warn!("config: remote skill index {url} lists no usable skills"),
            Err(error) => tracing::warn!("config: skipped remote skill index {url}: {error}"),
        }
    }
    roots
}

fn validate_asset_name(name: &str, kind: &str) -> Result<()> {
    if name.is_empty()
        || name.split(':').any(|part| {
            part.is_empty()
                || !part.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
                })
        })
    {
        anyhow::bail!(
            "{kind} name must contain colon-separated ASCII letters, digits, hyphens, or underscores"
        );
    }
    Ok(())
}

fn validate_qualified_model(model: &str, kind: &str) -> Result<()> {
    if matches!(model, "inherit" | "sonnet" | "opus" | "haiku") {
        return Ok(());
    }
    if model
        .split_once('/')
        .is_none_or(|(provider, model)| provider.trim().is_empty() || model.trim().is_empty())
    {
        anyhow::bail!("{kind} model must use provider/model form");
    }
    Ok(())
}

fn managed_name_path(root: &Path, name: &str) -> Result<PathBuf> {
    validate_asset_name(name, "asset")?;
    Ok(name
        .split(':')
        .fold(root.to_path_buf(), |path, part| path.join(part)))
}

fn managed_asset_path(
    paths: &Paths,
    workspace: &Path,
    project: bool,
    kind: &str,
    name: &str,
    extension: &str,
) -> Result<PathBuf> {
    let root = if project {
        workspace.join(".haarness").join(kind)
    } else {
        paths.config.parent().unwrap_or(Path::new(".")).join(kind)
    };
    let mut target = managed_name_path(&root, name)?;
    target.set_extension(extension);
    Ok(target)
}

fn asset_body(inline: Option<String>, file: Option<PathBuf>, label: &str) -> Result<String> {
    let body = match (inline, file) {
        (Some(body), None) => body,
        (None, Some(path)) => fs::read_to_string(&path)
            .with_context(|| format!("read {label} from {}", path.display()))?,
        (None, None) => anyhow::bail!("provide --file or the inline {label} option"),
        (Some(_), Some(_)) => unreachable!("clap rejects conflicting body inputs"),
    };
    if body.trim().is_empty() {
        anyhow::bail!("{label} must not be empty");
    }
    Ok(body)
}

fn parse_agent_tools(values: &[String]) -> Result<std::collections::HashMap<String, bool>> {
    let mut tools = std::collections::HashMap::new();
    for value in values {
        let (name, enabled) = value
            .rsplit_once('=')
            .with_context(|| format!("agent tool must be NAME=true|false: {value}"))?;
        if name.trim().is_empty() {
            anyhow::bail!("agent tool name cannot be empty: {value}");
        }
        let enabled = match enabled {
            "true" => true,
            "false" => false,
            _ => anyhow::bail!("agent tool must end in =true or =false: {value}"),
        };
        tools.insert(name.trim().to_owned(), enabled);
    }
    Ok(tools)
}

fn parse_agent_permissions(values: &[String]) -> Result<Vec<haarness_core::PermissionRule>> {
    use haarness_core::{PermissionDecision, PermissionRule};

    values
        .iter()
        .map(|value| {
            let (pattern, decision) = value.rsplit_once('=').with_context(|| {
                format!("agent permission must be PATTERN=allow|ask|deny: {value}")
            })?;
            if pattern.trim().is_empty() {
                anyhow::bail!("agent permission pattern cannot be empty: {value}");
            }
            let decision = match decision {
                "allow" => PermissionDecision::Allow,
                "ask" => PermissionDecision::Ask,
                "deny" => PermissionDecision::Deny,
                _ => anyhow::bail!("agent permission must end in =allow, =ask, or =deny: {value}"),
            };
            Ok(PermissionRule {
                pattern: pattern.trim().to_owned(),
                decision,
            })
        })
        .collect()
}

fn write_markdown_asset(
    path: &Path,
    metadata: &mut [(&str, Option<&str>)],
    body: &str,
    force: bool,
) -> Result<()> {
    if path.exists() && !force {
        anyhow::bail!(
            "{} already exists; pass --force to replace it",
            path.display()
        );
    }
    let parent = path
        .parent()
        .context("managed asset has no parent directory")?;
    fs::create_dir_all(parent)?;
    let mut content = String::from("---\n");
    for (name, value) in metadata.iter_mut() {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            use std::fmt::Write as _;
            writeln!(content, "{name}: {}", one_line_metadata(value))?;
        }
    }
    content.push_str("---\n");
    content.push_str(body.trim());
    content.push('\n');
    fs::write(path, content).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn one_line_metadata(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(['\r', '\n'], " ")
}

fn remove_managed_asset(path: &Path, directory: bool) -> Result<()> {
    if !path.exists() {
        anyhow::bail!("managed asset does not exist: {}", path.display());
    }
    if directory {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn reload_notice(paths: &Paths) {
    if read_runtime(paths).is_ok() {
        println!("Restart the workspace server to reload asset changes.");
    }
}

#[allow(clippy::too_many_arguments)]
async fn btw_command(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    yolo: bool,
    thread_id: ThreadId,
    question: String,
    json_output: bool,
) -> Result<()> {
    let discovery = ensure_workspace_server(paths, workspace, trust, yolo).await?;
    let client = haarness_tui::api::ApiClient::new(&discovery.url, &discovery.token)?;
    let response = client.btw(thread_id, question).await?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        println!("{}", response.answer);
    }
    Ok(())
}

async fn session_command(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    yolo: bool,
    command: SessionCommand,
) -> Result<()> {
    let discovery = ensure_workspace_server(paths, workspace, trust, yolo).await?;
    let client = haarness_tui::api::ApiClient::new(&discovery.url, &discovery.token)?;
    match command {
        SessionCommand::List { all, json } => {
            let sessions = if all {
                client.threads_including_archived().await?
            } else {
                client.threads().await?
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&sessions)?);
            } else if sessions.is_empty() {
                println!("No sessions.");
            } else {
                for session in sessions {
                    println!(
                        "{}\t{}\t{}/{}\t{}{}",
                        session.id,
                        session.title,
                        session.provider,
                        session.model,
                        session.updated_at.to_rfc3339(),
                        if session.archived { "\tarchived" } else { "" }
                    );
                }
            }
        }
        SessionCommand::Get { id, transcript } => {
            let id = ThreadId(id);
            let session = client.thread(id).await?;
            if transcript {
                let items = client.items(id).await?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({"session": session, "items": items}))?
                );
            } else {
                println!("{}", serde_json::to_string_pretty(&session)?);
            }
        }
        SessionCommand::Archive { id } => {
            let session = client
                .update_thread(ThreadId(id), session_archive_request(true))
                .await?;
            println!("Archived {} ({})", session.title, session.id);
        }
        SessionCommand::Unarchive { id } => {
            let session = client
                .update_thread(ThreadId(id), session_archive_request(false))
                .await?;
            println!("Unarchived {} ({})", session.title, session.id);
        }
        SessionCommand::Fork { id, title, at } => {
            let session = client
                .fork_thread_with(
                    ThreadId(id),
                    haarness_protocol::ForkThreadRequest {
                        title,
                        up_to_sequence: at,
                        before_item_id: None,
                    },
                )
                .await?;
            println!("{}", serde_json::to_string_pretty(&session)?);
        }
        SessionCommand::Export {
            id,
            sanitize,
            output,
            force,
        } => {
            let info = client.info().await?;
            if !info
                .capabilities
                .iter()
                .any(|capability| capability == SESSION_ARCHIVE_CAPABILITY)
            {
                anyhow::bail!(
                    "the connected Haarness server does not support {SESSION_ARCHIVE_CAPABILITY}"
                );
            }
            let archive = client
                .export_session_archive(ThreadId(id), sanitize)
                .await?;
            let mut payload = serde_json::to_vec_pretty(&archive)?;
            payload.push(b'\n');
            if let Some(path) = output {
                let mut options = fs::OpenOptions::new();
                options.write(true);
                if force {
                    options.create(true).truncate(true);
                } else {
                    options.create_new(true);
                }
                let mut file = options.open(&path).with_context(|| {
                    format!("failed to create session archive {}", path.display())
                })?;
                set_private(&file)?;
                file.write_all(&payload)?;
                println!(
                    "Exported {} session{} to {}{}",
                    archive.threads.len(),
                    if archive.threads.len() == 1 { "" } else { "s" },
                    path.display(),
                    if archive.sanitized {
                        " (sanitized)"
                    } else {
                        ""
                    }
                );
            } else {
                io::stdout().write_all(&payload)?;
            }
        }
        SessionCommand::Import { file, json } => {
            let info = client.info().await?;
            if !info
                .capabilities
                .iter()
                .any(|capability| capability == SESSION_ARCHIVE_CAPABILITY)
            {
                anyhow::bail!(
                    "the connected Haarness server does not support {SESSION_ARCHIVE_CAPABILITY}"
                );
            }
            let payload = fs::read(&file)
                .with_context(|| format!("failed to read session archive {}", file.display()))?;
            let archive: SessionArchive = serde_json::from_slice(&payload)
                .with_context(|| format!("invalid session archive JSON in {}", file.display()))?;
            let result = client.import_session_archive(archive).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!(
                    "Imported root {}: {} created, {} merged",
                    result.root.id,
                    result.created.len(),
                    result.merged.len()
                );
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn workspace_command(
    paths: &Paths,
    current_workspace_id: &str,
    trust: bool,
    yolo: bool,
    thread: Option<Uuid>,
    continue_last: bool,
    command: WorkspaceCommand,
) -> Result<()> {
    let manager = paths.workspace_manager()?;
    match command {
        WorkspaceCommand::List { json } => {
            let response = manager.list(Some(current_workspace_id))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
                return Ok(());
            }
            for project in &response.projects {
                println!(
                    "project\t{}\t{}\t{}{}",
                    project.id,
                    project.name,
                    project.root,
                    if project.git { "\tgit" } else { "" }
                );
                for workspace in response
                    .workspaces
                    .iter()
                    .filter(|workspace| workspace.project_id == project.id)
                {
                    println!(
                        "  {}\t{}\t{}\t{}{}{}",
                        if workspace.id == current_workspace_id {
                            "*"
                        } else {
                            " "
                        },
                        workspace.id,
                        workspace.name,
                        workspace.path,
                        workspace
                            .branch
                            .as_deref()
                            .map_or_else(String::new, |branch| format!("\t{branch}")),
                        if workspace.exists { "" } else { "\tmissing" }
                    );
                }
            }
        }
        WorkspaceCommand::Create {
            name,
            from_ref,
            branch,
            path,
        } => {
            let response = manager.create_worktree(
                current_workspace_id,
                &WorkspaceCreateRequest {
                    name,
                    from_ref,
                    branch,
                    path: path.map(|path| path.display().to_string()),
                },
            )?;
            println!("{}", response.message);
            println!("{}", response.workspace.path);
        }
        WorkspaceCommand::Open {
            reference,
            path_only,
        } => {
            let workspace = manager.open_workspace(&reference)?;
            if path_only {
                println!("{}", workspace.path);
            } else {
                let target_paths = paths.scoped(&workspace.id, false)?;
                interactive(
                    &target_paths,
                    PathBuf::from(&workspace.path),
                    trust,
                    yolo,
                    thread,
                    continue_last,
                )
                .await?;
            }
        }
        WorkspaceCommand::Reset {
            reference,
            confirm,
            discard_changes,
            remove_untracked,
            to_ref,
        } => {
            let response = manager.reset_worktree(
                &reference,
                &WorkspaceResetRequest {
                    confirm,
                    discard_changes,
                    remove_untracked,
                    to_ref,
                },
            )?;
            println!("{}", response.message);
        }
        WorkspaceCommand::Remove {
            reference,
            confirm,
            discard_changes,
            delete_branch,
        } => {
            let response = manager.remove_worktree(
                &reference,
                &WorkspaceRemoveRequest {
                    confirm,
                    discard_changes,
                    delete_branch,
                },
            )?;
            println!("{}", response.message);
        }
    }
    Ok(())
}

fn session_archive_request(archived: bool) -> UpdateThreadRequest {
    UpdateThreadRequest {
        title: None,
        archived: Some(archived),
        agent: None,
        provider: None,
        model: None,
        ultracode: None,
    }
}

async fn workflow_command(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    yolo: bool,
    command: WorkflowCommand,
) -> Result<()> {
    // Workflow mutations belong to the durable server too. In particular,
    // `workflow run --detach` must not be a task spawned inside a process that
    // exits immediately afterwards.
    let discovery = ensure_workspace_server(paths, workspace, trust, yolo).await?;
    let client = haarness_tui::api::ApiClient::new(&discovery.url, &discovery.token)?;

    match command {
        WorkflowCommand::List => {
            let catalog = client.workflows().await?;
            for definition in catalog.workflows {
                let description = definition.meta.description.as_deref().unwrap_or("");
                if description.is_empty() {
                    println!("{}\t{}", definition.invocation, definition.path);
                } else {
                    println!(
                        "{}\t{}\t{}",
                        definition.invocation, description, definition.path
                    );
                }
            }
            for invalid in catalog.invalid {
                eprintln!("invalid workflow {}: {}", invalid.path, invalid.error);
            }
        }
        WorkflowCommand::Source { name } => {
            print!("{}", client.workflow_source(&name).await?.source);
        }
        WorkflowCommand::Runs { limit } => {
            for run in client.workflow_runs(limit.clamp(1, 500)).await? {
                println!(
                    "{}\t{}\t{}\t{}",
                    run.id,
                    workflow_status(run.status),
                    run.workflow_name,
                    run.time.updated.to_rfc3339()
                );
            }
        }
        WorkflowCommand::Run {
            name,
            args,
            agent,
            model,
            variant,
            detach,
            timeout_ms,
        } => {
            let args = args
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .context("--args must be valid JSON")?;
            let started = client
                .start_workflow(WorkflowStartRequest {
                    name: Some(name),
                    args,
                    agent,
                    model,
                    variant,
                    // Always detach at the transport boundary. Synchronous
                    // CLI behavior is a client-side wait, so a client timeout
                    // or exit can never cancel the server-owned run.
                    wait: false,
                    timeout_ms: None,
                    ..WorkflowStartRequest::default()
                })
                .await?;
            if detach {
                print_workflow_run(&started)?;
            } else {
                let deadline =
                    timeout_ms.map(|timeout| Instant::now() + Duration::from_millis(timeout));
                let run = loop {
                    let run = client.workflow_run(started.id, None).await?;
                    if run.status != haarness_protocol::WorkflowRunStatus::Running {
                        break run;
                    }
                    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        anyhow::bail!(
                            "workflow wait timed out; {} continues on the Haarness server (run {})",
                            run.workflow_name,
                            run.id
                        );
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                };
                print_workflow_run(&run)?;
            }
        }
        WorkflowCommand::Inspect { run_id } => {
            print_workflow_run(&client.workflow_run(WorkflowId(run_id), None).await?)?;
        }
        WorkflowCommand::Cancel { run_id } => {
            print_workflow_run(&client.cancel_workflow(WorkflowId(run_id)).await?)?;
        }
        WorkflowCommand::Pause { run_id } => {
            print_workflow_run(&client.pause_workflow(WorkflowId(run_id)).await?)?;
        }
        WorkflowCommand::Resume { run_id } => {
            print_workflow_run(&client.resume_workflow(WorkflowId(run_id)).await?)?;
        }
        WorkflowCommand::Delete { run_id } => {
            client.delete_workflow(WorkflowId(run_id)).await?;
            println!("deleted {run_id}");
        }
    }
    Ok(())
}

async fn schedule_command(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    yolo: bool,
    project_config_override: Option<bool>,
    command: ScheduleCommand,
) -> Result<()> {
    let scheduler = Scheduler::discover(std::env::current_exe()?, paths.root.clone())?;
    match command {
        ScheduleCommand::List { json } => {
            let jobs = scheduler.list()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&jobs)?);
            } else if jobs.is_empty() {
                println!("No schedules configured.");
            } else {
                for schedule in jobs {
                    println!("{} {}", schedule.id, Scheduler::summary(&schedule));
                }
            }
        }
        ScheduleCommand::Get { reference } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&scheduler.require(&reference)?)?
            );
        }
        ScheduleCommand::Add(args) => {
            let schedule = scheduler.create(CreateSchedule {
                name: args.name,
                prompt: args.prompt,
                request: args.natural,
                workspace: args.directory.unwrap_or_else(|| workspace.to_path_buf()),
                model: args.model,
                agent: args.agent,
                variant: args.variant,
                trust_project: trust,
                enabled: !args.disabled,
                trigger: schedule_trigger(args.every, args.cron, args.at)?,
                timezone: args.timezone,
            })?;
            println!("{} {}", schedule.id, Scheduler::summary(&schedule));
        }
        ScheduleCommand::Update(args) => {
            let enabled = args
                .enable
                .then_some(true)
                .or(args.disable.then_some(false));
            let schedule = scheduler.update(
                &args.reference,
                UpdateSchedule {
                    name: args.name,
                    prompt: args.prompt,
                    request: args.natural,
                    workspace: args.directory,
                    model: args.model.map(Some),
                    agent: args.agent.map(Some),
                    variant: args.variant.map(Some),
                    trust_project: project_config_override,
                    enabled,
                    trigger: schedule_trigger(args.every, args.cron, args.at)?,
                    timezone: args.timezone.map(Some),
                },
            )?;
            println!("{} {}", schedule.id, Scheduler::summary(&schedule));
        }
        ScheduleCommand::Remove { reference } => {
            let schedule = scheduler.remove(&reference)?;
            println!("Removed {}", schedule.name);
        }
        ScheduleCommand::Enable { reference } => {
            println!("Enabled {}", scheduler.toggle(&reference, true)?.name);
        }
        ScheduleCommand::Disable { reference } => {
            println!("Disabled {}", scheduler.toggle(&reference, false)?.name);
        }
        ScheduleCommand::Run { reference } => {
            let scheduler = scheduler.clone();
            let run = tokio::task::spawn_blocking(move || scheduler.run_now(&reference)).await??;
            println!("{}", serde_json::to_string_pretty(&run)?);
            if run.status == ScheduleRunStatus::Failed {
                anyhow::bail!(
                    "scheduled run failed: {}",
                    run.error.as_deref().unwrap_or("unknown error")
                );
            }
        }
        ScheduleCommand::History {
            reference,
            limit,
            json,
        } => {
            let runs = scheduler.history(&reference, limit.clamp(1, 500))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&runs)?);
            } else if runs.is_empty() {
                println!("No runs recorded yet.");
            } else {
                for run in runs {
                    println!(
                        "{:?}\t{}\t{}\t{}",
                        run.status,
                        run.finished_at.to_rfc3339(),
                        run.thread_id.map_or_else(String::new, |id| id.to_string()),
                        run.error.unwrap_or_default()
                    );
                }
            }
        }
        ScheduleCommand::Logs { reference } => {
            let is_schedule_log = reference.is_some();
            let path = reference.map_or_else(
                || Ok(scheduler.logs_dir()),
                |reference| {
                    scheduler
                        .require(&reference)
                        .map(|schedule| scheduler.log_path(schedule.id))
                },
            )?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            println!("{}", path.display());
            #[cfg(target_os = "macos")]
            {
                let reveal = if is_schedule_log && path.is_file() {
                    ProcessCommand::new("open").arg("-R").arg(&path).status()
                } else {
                    let directory = if is_schedule_log {
                        path.parent().unwrap_or(&path)
                    } else {
                        &path
                    };
                    fs::create_dir_all(directory)?;
                    ProcessCommand::new("open").arg(directory).status()
                };
                if let Err(error) = reveal {
                    tracing::debug!(%error, "could not reveal schedule logs");
                }
            }
        }
        ScheduleCommand::Trigger { reference, source } => {
            let run = execute_schedule(paths, &scheduler, &reference, source, yolo, trust).await?;
            println!("{}", serde_json::to_string(&run)?);
            if run.status == ScheduleRunStatus::Failed {
                anyhow::bail!(
                    "scheduled run failed: {}",
                    run.error.as_deref().unwrap_or("unknown error")
                );
            }
        }
    }
    Ok(())
}

fn schedule_trigger(
    every: Option<String>,
    cron: Option<String>,
    at: Option<String>,
) -> Result<Option<ScheduleTrigger>> {
    if let Some(value) = every {
        let value = value.trim().to_lowercase();
        let split = value
            .find(|character: char| !character.is_ascii_digit())
            .context("--every must look like 5m, 2h, or 1d")?;
        let amount = value[..split].parse::<u64>()?;
        let unit = value[split..].trim();
        let factor = match unit {
            "s" | "sec" | "second" | "seconds" => 1,
            "m" | "min" | "minute" | "minutes" => 60,
            "h" | "hour" | "hours" => 60 * 60,
            "d" | "day" | "days" => 60 * 60 * 24,
            _ => anyhow::bail!("invalid --every unit: {unit}"),
        };
        return Ok(Some(ScheduleTrigger::Interval {
            seconds: amount
                .checked_mul(factor)
                .context("schedule interval is too large")?,
        }));
    }
    if let Some(expression) = cron {
        return Ok(Some(ScheduleTrigger::Cron { expression }));
    }
    if let Some(at) = at {
        let at = chrono::DateTime::parse_from_rfc3339(&at)
            .map(|value| value.with_timezone(&chrono::Utc))
            .with_context(
                || "--at must be an RFC3339 timestamp such as 2026-08-27T09:00:00+09:00",
            )?;
        return Ok(Some(ScheduleTrigger::Once { at }));
    }
    Ok(None)
}

async fn execute_schedule(
    paths: &Paths,
    scheduler: &Scheduler,
    reference: &str,
    source: ScheduleRunSource,
    yolo: bool,
    trust: bool,
) -> Result<ScheduleRun> {
    let schedule = scheduler.require(reference)?;
    let started_at = chrono::Utc::now();
    if !schedule.enabled && source != ScheduleRunSource::Manual {
        return Ok(ScheduleRun {
            id: Uuid::now_v7(),
            schedule_id: schedule.id,
            status: ScheduleRunStatus::Skipped,
            source,
            started_at,
            finished_at: started_at,
            thread_id: None,
            turn_id: None,
            response: None,
            error: Some("Schedule is disabled.".into()),
        });
    }
    let Some(guard) = scheduler.acquire(&schedule)? else {
        let run = ScheduleRun {
            id: Uuid::now_v7(),
            schedule_id: schedule.id,
            status: ScheduleRunStatus::Skipped,
            source,
            started_at,
            finished_at: chrono::Utc::now(),
            thread_id: None,
            turn_id: None,
            response: None,
            error: Some("Skipped because a previous run is still active.".into()),
        };
        scheduler.record(&schedule, &run)?;
        return Ok(run);
    };

    let mut thread_id = None;
    let mut turn_id = None;
    let result = async {
        let discovery = ensure_workspace_server(
            paths,
            &schedule.workspace,
            schedule.trust_project && trust,
            yolo,
        )
        .await?;
        let client = haarness_tui::api::ApiClient::new(&discovery.url, &discovery.token)?;
        let (provider, model) = schedule
            .model
            .as_deref()
            .map(|value| {
                value
                    .split_once('/')
                    .map(|(provider, model)| (Some(provider.to_owned()), Some(model.to_owned())))
                    .context("scheduled model must use provider/model form")
            })
            .transpose()?
            .unwrap_or((None, None));
        let thread = client
            .create_thread(CreateThreadRequest {
                schedule: Some(haarness_protocol::ScheduleOrigin {
                    id: schedule.id,
                    name: schedule.name.clone(),
                }),
                title: Some(format!(
                    "Schedule: {} @ {}",
                    schedule.name,
                    chrono::Utc::now().to_rfc3339()
                )),
                parent_id: None,
                agent: schedule.agent.clone(),
                provider: provider.clone(),
                model: model.clone(),
                ultracode: None,
            })
            .await?;
        thread_id = Some(thread.id.0);
        let turn = client
            .start_turn(
                thread.id,
                StartTurnRequest {
                    request_id: RequestId::new(),
                    text: schedule.prompt.clone(),
                    attachments: Vec::new(),
                    synthetic: false,
                    auto_approve: true,
                    agent: schedule.agent.clone(),
                    provider,
                    model,
                    reasoning_effort: schedule.variant.clone(),
                    tools: Some(HashMap::from([("question".into(), false)])),
                    command: None,
                },
            )
            .await?;
        turn_id = Some(turn.id.0);
        wait_for_turn_client(&client, turn.id).await?;
        let response =
            assistant_messages_for_turn(client.items(thread.id).await?, turn.id).join("\n");
        Result::<String>::Ok(response)
    }
    .await;

    let run = match result {
        Ok(response) => ScheduleRun {
            id: Uuid::now_v7(),
            schedule_id: schedule.id,
            status: ScheduleRunStatus::Success,
            source,
            started_at,
            finished_at: chrono::Utc::now(),
            thread_id,
            turn_id,
            response: Some(response),
            error: None,
        },
        Err(error) => ScheduleRun {
            id: Uuid::now_v7(),
            schedule_id: schedule.id,
            status: ScheduleRunStatus::Failed,
            source,
            started_at,
            finished_at: chrono::Utc::now(),
            thread_id,
            turn_id,
            response: None,
            error: Some(format!("{error:#}")),
        },
    };
    scheduler.finish(&schedule, run, guard)
}

fn print_workflow_run(run: &WorkflowRun) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(run)?);
    Ok(())
}

fn workflow_status(status: haarness_protocol::WorkflowRunStatus) -> &'static str {
    use haarness_protocol::WorkflowRunStatus;
    match status {
        WorkflowRunStatus::Running => "running",
        WorkflowRunStatus::Completed => "completed",
        WorkflowRunStatus::Failed => "failed",
        WorkflowRunStatus::Cancelled => "cancelled",
        WorkflowRunStatus::Paused => "paused",
        WorkflowRunStatus::Interrupted => "interrupted",
    }
}

/// Re-submit loop prompts: each completed turn on a looped thread schedules
/// the next submission after the loop interval; expired loops clear. Armed
/// timers dedup so a boot re-arm and a live finish never double-fire.
fn spawn_loop_driver(runtime: Arc<AgentRuntime>) {
    tokio::spawn(async move {
        let armed: Arc<parking_lot::Mutex<std::collections::HashSet<String>>> =
            Arc::new(parking_lot::Mutex::new(std::collections::HashSet::new()));
        // Boot re-arm for threads that already loop.
        if let Ok(threads) = runtime.list_threads(false).await {
            for thread in threads {
                arm_loop(&runtime, thread.id, &armed).await;
            }
        }
        let subscription = runtime.events().subscribe(None);
        let mut receiver = subscription.receiver;
        loop {
            match receiver.recv().await {
                Ok(event) => {
                    if event.event_type == "turn.finished" {
                        if let Some(thread_id) = event.thread_id {
                            arm_loop(&runtime, thread_id, &armed).await;
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

async fn arm_loop(
    runtime: &Arc<AgentRuntime>,
    thread_id: haarness_protocol::ThreadId,
    armed: &Arc<parking_lot::Mutex<std::collections::HashSet<String>>>,
) {
    let Ok((_, Some(config))) = runtime.thread_extras(thread_id).await else {
        return;
    };
    if config.expires_at < chrono::Utc::now() {
        let _ = runtime.set_loop(thread_id, None).await;
        tracing::info!(thread = %thread_id, "loop expired");
        return;
    }
    if !armed.lock().insert(thread_id.to_string()) {
        return;
    }
    let runtime = runtime.clone();
    let armed = armed.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(config.interval_seconds)).await;
        armed.lock().remove(&thread_id.to_string());
        // Re-check: the loop may have been stopped while we slept.
        let Ok((_, Some(config))) = runtime.thread_extras(thread_id).await else {
            return;
        };
        if config.expires_at < chrono::Utc::now() {
            let _ = runtime.set_loop(thread_id, None).await;
            return;
        }
        let request = StartTurnRequest {
            request_id: RequestId::new(),
            text: config.prompt,
            attachments: vec![],
            synthetic: false,
            auto_approve: false,
            agent: None,
            provider: None,
            model: None,
            reasoning_effort: None,
            tools: None,
            command: None,
        };
        match runtime.start_turn(thread_id, request).await {
            Ok(_) => tracing::info!(thread = %thread_id, "loop prompt submitted"),
            Err(error) => tracing::debug!(thread = %thread_id, %error, "loop submit skipped"),
        }
    });
}

async fn interactive(
    paths: &Paths,
    workspace: PathBuf,
    trust: bool,
    yolo: bool,
    thread: Option<Uuid>,
    continue_last: bool,
) -> Result<()> {
    let manager = paths.workspace_manager()?;
    let mut active_paths = paths.clone();
    let mut active_workspace = workspace;
    let mut initial_thread = thread;
    let mut resume = continue_last;
    loop {
        let discovery =
            ensure_workspace_server(&active_paths, &active_workspace, trust, yolo).await?;
        let selected = haarness_tui::run_with_thread_and_resolver_switching(
            &discovery.url,
            &discovery.token,
            initial_thread,
            resume,
            runtime_endpoint_resolver(&active_paths, &active_workspace),
        )
        .await?;
        let Some(path) = selected else {
            return Ok(());
        };
        let workspace = manager.register_local(&path)?;
        active_paths = paths.scoped(&workspace.id, false)?;
        active_workspace = PathBuf::from(workspace.path);
        // An explicit session belongs to the workspace it was supplied for;
        // switching restores the target's own most-recent root instead.
        initial_thread = None;
        resume = true;
    }
}
async fn serve_command(
    paths: &Paths,
    workspace: PathBuf,
    trust: bool,
    yolo: bool,
    args: ServeArgs,
) -> Result<()> {
    if args.stop {
        stop_workspace_server(paths, &workspace).await?;
        return Ok(());
    }
    let token = args.token.filter(|token| !token.trim().is_empty());
    let access = if let Some(password) = args
        .authentication
        .password
        .filter(|value| !value.is_empty())
    {
        let username = args
            .authentication
            .username
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "haarness".into());
        let _ = haarness_tui::api::ServerAuth::basic(&username, &password)?;
        haarness_server::auth::ServerAccess::Password {
            username: username.trim().into(),
            password,
        }
    } else if args.anonymous || token.is_none() {
        haarness_server::auth::ServerAccess::Open
    } else {
        haarness_server::auth::ServerAccess::Private
    };
    if args.daemon && !args.background_child {
        let discovery = ensure_workspace_server_at(
            paths,
            &workspace,
            trust,
            yolo,
            SocketAddr::new(args.host, args.port),
            token.unwrap_or_else(random_token),
            Some(access),
        )
        .await?;
        println!(
            "Haarness server {} is running at {} for {}",
            discovery.pid,
            discovery.url,
            workspace.display()
        );
        return Ok(());
    }
    let _server_lease = acquire_server_lease(paths)?;
    if let Some(live) = inspect_workspace_server(paths, &workspace).await? {
        let discovery = live.discovery;
        anyhow::bail!(
            "Haarness server {} is already running at {} for {}",
            discovery.pid,
            discovery.url,
            workspace.display()
        );
    }
    let token = token.unwrap_or_else(random_token);
    let (listener, state, hot_reload) = application_at(
        paths,
        &workspace,
        trust,
        yolo,
        SocketAddr::new(args.host, args.port),
        true,
    )
    .await?;
    let address = listener.local_addr()?;
    let upstream_address = if address.ip().is_unspecified() {
        SocketAddr::new(
            if address.is_ipv6() {
                std::net::Ipv6Addr::LOCALHOST.into()
            } else {
                Ipv4Addr::LOCALHOST.into()
            },
            address.port(),
        )
    } else {
        address
    };
    let host = Arc::new(remote::Host {
        paths: paths.clone(),
        workspace: workspace.clone(),
        url: format!("http://{upstream_address}"),
        token: token.clone(),
        trust,
        locks: tokio::sync::Mutex::new(std::collections::HashMap::new()),
    });
    let access_fingerprint = Some(access.fingerprint(&token));
    let handle =
        haarness_server::api::serve_with_access(listener, &token, state, Some(host), access)
            .await?;
    tokio::spawn(hot_reload.run(handle.shutdown_signal()));
    write_runtime(
        paths,
        &RuntimeDiscovery {
            url: format!("http://{}", handle.address),
            token,
            pid: std::process::id(),
            workspace: Some(workspace.display().to_string()),
            executable_fingerprint: Some(current_executable_fingerprint()?),
            access_fingerprint,
        },
    )?;
    if !args.background_child {
        println!("Haarness server listening on http://{}", handle.address);
    }
    let shutdown = handle.shutdown_signal();
    wait_for_shutdown(shutdown.clone()).await?;
    shutdown.cancel();
    let result = handle.wait().await;
    remove_runtime_if_owner(paths, std::process::id());
    result?;
    Ok(())
}

async fn attach_command(
    paths: &Paths,
    workspace: &Path,
    args: AttachArgs,
    thread: Option<Uuid>,
    continue_last: bool,
) -> Result<()> {
    if args.fork && thread.is_none() && !continue_last {
        anyhow::bail!("--fork requires --session/--thread or --continue");
    }
    let (server, token) =
        attached_endpoint(paths, &args.url, args.token, args.authentication).await?;
    haarness_tui::run_attached_with_auth(&server, token, thread, continue_last, args.fork).await?;
    // Keep the local workspace argument meaningful for diagnostics and parity
    // with the other entrypoints; execution itself belongs to server /v1/info.
    tracing::debug!(workspace = %workspace.display(), server = %server, "attached TUI exited");
    Ok(())
}

/// Stop the discovered workspace owner. A reachable server gets its normal
/// authenticated drain. Explicit `serve --stop` also recovers the narrower
/// failure mode where discovery and the workspace lease still belong to a
/// Haarness process whose listener has already disappeared.
async fn stop_workspace_server(paths: &Paths, workspace: &Path) -> Result<()> {
    if let Some(live) = inspect_workspace_server(paths, workspace).await? {
        let discovery = live.discovery;
        haarness_tui::api::ApiClient::new(&discovery.url, &discovery.token)?
            .shutdown_server()
            .await?;
        for _ in 0..100 {
            if inspect_workspace_server(paths, workspace).await?.is_none() {
                // The listener closes just before the server process finishes
                // its final drain and drops the workspace lease.
                tokio::time::sleep(Duration::from_millis(100)).await;
                println!("Stopped Haarness server {}.", discovery.pid);
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        anyhow::bail!("server {} did not stop within 5 seconds", discovery.pid);
    }

    let discovery = match read_runtime(paths) {
        Ok(discovery) => discovery,
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::NotFound) =>
        {
            println!("No Haarness server is running for {}.", workspace.display());
            return Ok(());
        }
        Err(error) => return Err(error).context("cannot read stale runtime discovery"),
    };
    if discovery.workspace.as_deref().is_some_and(|actual| {
        comparable_workspace(Path::new(actual)) != comparable_workspace(workspace)
    }) {
        anyhow::bail!(
            "runtime discovery belongs to a different workspace; refusing to stop process {}",
            discovery.pid
        );
    }

    let lease = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&paths.server_lock)
        .with_context(|| format!("cannot open {}", paths.server_lock.display()))?;
    match lease.try_lock_exclusive() {
        Ok(()) => {
            // No process owns the workspace lease: discovery is only stale
            // metadata, so never signal a potentially reused PID.
            remove_runtime_if_owner(paths, discovery.pid);
            println!(
                "Removed stale Haarness discovery for {}.",
                workspace.display()
            );
            return Ok(());
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
        Err(error) => return Err(error).context("cannot inspect the Haarness workspace lease"),
    }

    terminate_unreachable_server(discovery.pid).await?;
    remove_runtime_if_owner(paths, discovery.pid);
    println!("Stopped unresponsive Haarness server {}.", discovery.pid);
    Ok(())
}

#[cfg(unix)]
async fn terminate_unreachable_server(pid: u32) -> Result<()> {
    use nix::{
        errno::Errno,
        sys::signal::{Signal, kill},
        unistd::Pid,
    };

    let raw = i32::try_from(pid).context("server PID exceeds the platform process-id range")?;
    let target = Pid::from_raw(raw);
    let alive = || !matches!(kill(target, None), Err(Errno::ESRCH));
    match kill(target, Signal::SIGTERM) {
        Ok(()) | Err(Errno::ESRCH) => {}
        Err(error) => anyhow::bail!("cannot terminate unresponsive server {pid}: {error}"),
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if alive() {
        kill(target, Signal::SIGKILL)
            .with_context(|| format!("cannot kill unresponsive server {pid}"))?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while alive() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    if alive() {
        anyhow::bail!("unresponsive server {pid} did not exit after SIGKILL");
    }
    Ok(())
}

#[cfg(not(unix))]
async fn terminate_unreachable_server(pid: u32) -> Result<()> {
    anyhow::bail!(
        "Haarness server {pid} owns the workspace lease but is unreachable; stop that process with the platform task manager"
    )
}
async fn tui_command(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    yolo: bool,
    args: TuiArgs,
    thread: Option<Uuid>,
    continue_last: bool,
) -> Result<()> {
    if args.fork && thread.is_none() && !continue_last {
        anyhow::bail!("--fork requires --session/--thread or --continue");
    }
    if let Some(server) = args.connect {
        let (server, token) =
            attached_endpoint(paths, &server, args.token, args.authentication).await?;
        return haarness_tui::run_attached_with_auth(
            &server,
            token,
            thread,
            continue_last,
            args.fork,
        )
        .await;
    }
    let manager = paths.workspace_manager()?;
    let mut active_paths = paths.clone();
    let mut active_workspace = workspace.to_path_buf();
    let mut initial_thread = thread;
    let mut resume = continue_last;
    let mut fork = args.fork;
    loop {
        let discovery =
            ensure_workspace_server(&active_paths, &active_workspace, trust, yolo).await?;
        let selected = haarness_tui::run_with_thread_and_resolver_fork_switching(
            &discovery.url,
            &discovery.token,
            initial_thread,
            resume,
            fork,
            runtime_endpoint_resolver(&active_paths, &active_workspace),
        )
        .await?;
        let Some(path) = selected else {
            return Ok(());
        };
        let workspace = manager.register_local(&path)?;
        active_paths = paths.scoped(&workspace.id, false)?;
        active_workspace = PathBuf::from(workspace.path);
        initial_thread = None;
        resume = true;
        fork = false;
    }
}
/// Whether reading stdin would return immediately (data waiting, or the
/// writer already closed it). Non-Unix targets keep the old always-read path.
#[cfg(unix)]
fn stdin_ready_now() -> bool {
    use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
    use std::os::fd::AsFd as _;
    let stdin = io::stdin();
    let mut fds = [PollFd::new(stdin.as_fd(), PollFlags::POLLIN)];
    match poll(&mut fds, PollTimeout::ZERO) {
        Ok(0) => false,
        Ok(_) => fds[0].revents().is_some_and(|flags| {
            flags.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR)
        }),
        Err(_) => true,
    }
}

#[cfg(not(unix))]
fn stdin_ready_now() -> bool {
    true
}

async fn run_command(
    paths: &Paths,
    workspace: PathBuf,
    trust: bool,
    yolo: bool,
    requested_thread: Option<Uuid>,
    continue_last: bool,
    args: RunArgs,
) -> Result<()> {
    let show_thinking = args.show_thinking();
    let mut prompt = args.prompt.join(" ");
    // Piped context augments the prompt, but an idle pipe (a spawner that
    // never closes stdin) must not hold a prompted run hostage: with a prompt
    // on the command line, only stdin that already has data or EOF is read.
    if !io::stdin().is_terminal() && (args.prompt.is_empty() || stdin_ready_now()) {
        let mut stdin = String::new();
        io::stdin().read_to_string(&mut stdin)?;
        if !stdin.is_empty() {
            if !args.prompt.is_empty() {
                prompt.push('\n');
            }
            // Piped context augments positional
            // instructions instead of being silently discarded.
            prompt.push_str(&stdin);
        }
    }
    prompt = prompt.trim().to_owned();
    if prompt.is_empty() && args.command.is_none() && args.workflow.is_none() {
        anyhow::bail!("a prompt argument, --command, --workflow, or piped stdin is required");
    }
    if args.background_timeout_ms == Some(0) {
        anyhow::bail!("--background-timeout-ms must be a positive integer");
    }
    let (provider, model) = qualified_run_model(&args.provider, &args.model)?;
    let attachments = args
        .files
        .iter()
        .map(|path| {
            path.canonicalize()
                .with_context(|| format!("cannot read attachment {}", path.display()))
                .map(|path| path.display().to_string())
        })
        .collect::<Result<Vec<_>>>()?;
    if args.workflow.is_some() && !attachments.is_empty() {
        anyhow::bail!(
            "--file cannot be combined with --workflow; pass paths through workflow arguments"
        )
    }
    let client = if let Some(server) = args.attach.as_deref() {
        let (server, token) = attached_endpoint(
            paths,
            server,
            args.token.clone(),
            args.authentication.clone(),
        )
        .await?;
        haarness_tui::api::ApiClient::with_auth(&server, token)?.with_attachment_uploads()
    } else {
        let discovery = ensure_workspace_server(paths, &workspace, trust, yolo).await?;
        haarness_tui::api::ApiClient::new(&discovery.url, &discovery.token)?
    };
    let thread = select_run_thread_client(
        &client,
        requested_thread,
        continue_last,
        args.fork,
        CreateThreadRequest {
            schedule: None,
            title: Some(run_title(args.title.as_deref(), &prompt)),
            parent_id: None,
            agent: Some(args.agent.clone()),
            provider: Some(provider.clone()),
            model: Some(model.clone()),
            ultracode: None,
        },
    )
    .await?;
    if let Some(workflow) = args.workflow {
        let mut values = serde_json::Map::new();
        if !prompt.trim().is_empty() {
            values.insert("input".into(), serde_json::Value::String(prompt));
        }
        for argument in args.workflow_args {
            let (key, raw) = argument
                .split_once('=')
                .with_context(|| format!("workflow argument must be KEY=VALUE: {argument}"))?;
            if key.trim().is_empty() {
                anyhow::bail!("workflow argument key cannot be empty: {argument}");
            }
            let value = serde_json::from_str(raw)
                .unwrap_or_else(|_| serde_json::Value::String(raw.to_owned()));
            values.insert(key.trim().to_owned(), value);
        }
        let started = client
            .start_workflow(WorkflowStartRequest {
                name: Some(workflow),
                source: None,
                path: None,
                args: Some(serde_json::Value::Object(values)),
                agent: Some(args.agent.clone()),
                model: Some(format!("{provider}/{model}")),
                variant: args.variant.clone(),
                thread_id: Some(thread.id),
                wait: false,
                timeout_ms: None,
            })
            .await?;
        let run = loop {
            let run = client.workflow_run(started.id, None).await?;
            if run.status != haarness_protocol::WorkflowRunStatus::Running {
                break run;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        match run.status {
            haarness_protocol::WorkflowRunStatus::Completed => {
                if let Some(result) = run.result {
                    match result {
                        serde_json::Value::String(text) if args.format == RunFormat::Default => {
                            println!("{text}")
                        }
                        value if args.format == RunFormat::Default => {
                            println!("{}", serde_json::to_string_pretty(&value)?)
                        }
                        value => println!(
                            "{}",
                            json!({"type":"workflow", "runID":run.id, "result":value})
                        ),
                    }
                }
                return Ok(());
            }
            _ => anyhow::bail!(
                "workflow {} ended as {:?}: {}",
                run.workflow_name,
                run.status,
                run.error.unwrap_or_else(|| "no result".into())
            ),
        }
    }
    if args.goal {
        set_goal_for_run_client(&client, thread.id, prompt.clone()).await?;
    }
    let turn = client
        .start_turn(
            thread.id,
            StartTurnRequest {
                request_id: RequestId::new(),
                text: prompt,
                attachments,
                synthetic: false,
                auto_approve: yolo,
                agent: Some(args.agent),
                provider: Some(provider),
                model: Some(model),
                reasoning_effort: args.ultracode.then(|| "ultracode".into()).or(args.variant),
                tools: None,
                command: args
                    .command
                    .map(|command| command.trim_start_matches('/').to_owned()),
            },
        )
        .await?;
    let printed_through = if args.goal {
        let result = wait_for_goal_client(&client, thread.id, goal_idle_timeout()).await;
        let printed_through =
            print_run_output_client(&client, thread.id, turn.id, args.format, show_thinking)
                .await?;
        result?;
        printed_through
    } else {
        wait_for_turn_client(&client, turn.id).await?;
        print_run_output_client(&client, thread.id, turn.id, args.format, show_thinking).await?
    };
    settle_cli_subagents_client(
        &client,
        thread.id,
        args.wait_background || args.background_timeout_ms.is_some(),
        args.background_timeout_ms,
        args.format,
        show_thinking,
        printed_through,
    )
    .await?;
    settle_cli_workflows_client(
        &client,
        thread.id,
        args.wait_background || args.background_timeout_ms.is_some(),
        args.background_timeout_ms,
        args.format,
    )
    .await?;
    Ok(())
}

fn qualified_run_model(provider: &str, model: &str) -> Result<(String, String)> {
    if let Some((qualified_provider, qualified_model)) = model.split_once('/') {
        if qualified_provider.trim().is_empty() || qualified_model.trim().is_empty() {
            anyhow::bail!("--model must be a model id or provider/model");
        }
        // Cloud model IDs themselves contain an upstream namespace. An explicit
        // Cloud provider must not turn openai/gpt-* into a direct OpenAI login.
        if haarness_protocol::provider_storage_id(provider) == "haarness"
            && haarness_protocol::provider_storage_id(qualified_provider.trim()) != "haarness"
        {
            return Ok(("haarness".into(), model.trim().to_owned()));
        }
        return Ok((
            haarness_protocol::provider_storage_id(qualified_provider.trim()).to_owned(),
            qualified_model.trim().to_owned(),
        ));
    }
    Ok((
        haarness_protocol::provider_storage_id(provider).to_owned(),
        model.to_owned(),
    ))
}

#[cfg(test)]
#[test]
fn cloud_model_prefix_preserves_the_existing_provider_identity() {
    for prefix in ["haarness", "haarness-cloud"] {
        assert_eq!(
            qualified_run_model("openai", &format!("{prefix}/openai/gpt-6-sol")).unwrap(),
            ("haarness".into(), "openai/gpt-6-sol".into())
        );
        assert_eq!(
            qualified_run_model(prefix, "gpt-6-sol").unwrap().0,
            "haarness"
        );
        for model in ["openai/gpt-5-nano", "anthropic/claude-haiku-4.5", "google/gemini-3.5-flash-lite"] {
            assert_eq!(qualified_run_model(prefix, model).unwrap(), ("haarness".into(), model.into()));
            assert_eq!(qualified_run_model(prefix, &format!("{prefix}/{model}")).unwrap(), ("haarness".into(), model.into()));
        }
        assert!(qualified_run_model(prefix, "openai/").is_err());
    }
    assert_eq!(qualified_run_model("openai", "openai/gpt-5-nano").unwrap(), ("openai".into(), "gpt-5-nano".into()));
}

fn run_title(requested: Option<&str>, prompt: &str) -> String {
    if let Some(title) = requested.filter(|title| !title.trim().is_empty()) {
        return title.trim().to_owned();
    }
    let mut title = prompt.chars().take(50).collect::<String>();
    if title.trim().is_empty() {
        return "CLI run".into();
    }
    if prompt.chars().count() > 50 {
        title.push('…');
    }
    title
}

async fn select_run_thread_client(
    client: &haarness_tui::api::ApiClient,
    requested_thread: Option<Uuid>,
    continue_last: bool,
    fork: bool,
    create: CreateThreadRequest,
) -> Result<Thread> {
    if fork && requested_thread.is_none() && !continue_last {
        anyhow::bail!("--fork requires --session/--thread or --continue");
    }
    let base = if let Some(id) = requested_thread {
        Some(client.thread(ThreadId(id)).await?)
    } else if continue_last {
        client
            .threads()
            .await?
            .into_iter()
            .find(|thread| thread.parent_id.is_none() && !thread.archived)
    } else {
        None
    };
    match base {
        Some(thread) if fork => client.fork_thread(thread.id).await,
        Some(thread) => Ok(thread),
        None => client.create_thread(create).await,
    }
}

async fn set_goal_for_run_client(
    client: &haarness_tui::api::ApiClient,
    thread_id: ThreadId,
    objective: String,
) -> Result<()> {
    if client.active_turn(thread_id).await?.is_some() {
        anyhow::bail!(
            "cannot start a goal in session {thread_id} while its current turn is still running"
        );
    }
    client
        .goal_action(
            thread_id,
            &GoalActionRequest::Set {
                goal: objective,
                token_budget: None,
            },
        )
        .await?;
    Ok(())
}

async fn resolve_noninteractive_questions_client(
    client: &haarness_tui::api::ApiClient,
    root_thread_id: ThreadId,
) -> Result<()> {
    for question in client
        .pending_questions()
        .await?
        .into_iter()
        .filter(|question| {
            question.thread_id == root_thread_id || question.root_thread_id == Some(root_thread_id)
        })
    {
        let answer = if question.multiple && question.custom {
            "[]".into()
        } else {
            question
                .choices
                .iter()
                .find(|choice| choice.trim().eq_ignore_ascii_case("cancel"))
                .cloned()
                .unwrap_or_else(|| {
                    "Question rejected: this non-interactive run cannot answer questions.".into()
                })
        };
        eprintln!(
            "Question requested by thread {}; applying the non-interactive fallback.",
            question.thread_id
        );
        client
            .reply_question(&question.question_id, QuestionReply { answer })
            .await?;
    }
    Ok(())
}

async fn wait_for_turn_client(
    client: &haarness_tui::api::ApiClient,
    turn_id: TurnId,
) -> Result<()> {
    loop {
        let current = client.turn(turn_id).await?;
        resolve_noninteractive_questions_client(client, current.thread_id).await?;
        if !matches!(current.status, TurnStatus::Queued | TurnStatus::Running) {
            if current.status != TurnStatus::Completed {
                anyhow::bail!(current.error.unwrap_or_else(|| "turn failed".into()));
            }
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for_goal_client(
    client: &haarness_tui::api::ApiClient,
    thread_id: ThreadId,
    timeout: Duration,
) -> Result<()> {
    let mut idle_since = None;
    let mut last_updated = None;
    loop {
        resolve_noninteractive_questions_client(client, thread_id).await?;
        let state = client
            .goal(thread_id)
            .await?
            .context("goal was cleared before completion")?;
        if !state.status.is_active() {
            return finish_goal_run(&state);
        }
        if last_updated != Some(state.updated_at) {
            last_updated = Some(state.updated_at);
            idle_since = None;
        }
        if client.active_turn(thread_id).await?.is_some() {
            idle_since = None;
        } else {
            let since = idle_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= timeout {
                anyhow::bail!(
                    "goal watcher detached after {}s without a goal update; the server still owns the goal and it can be resumed from this session",
                    timeout.as_secs_f64()
                );
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn print_run_output_client(
    client: &haarness_tui::api::ApiClient,
    thread_id: ThreadId,
    turn_id: TurnId,
    format: RunFormat,
    thinking: bool,
) -> Result<u64> {
    let items = client.items(thread_id).await?;
    let printed_through = items
        .iter()
        .filter(|item| item.turn_id == Some(turn_id))
        .map(|item| item.sequence)
        .max()
        .unwrap_or(0);
    if format == RunFormat::Default && !thinking {
        for text in assistant_messages_for_turn(items, turn_id) {
            println!("{text}");
        }
        return Ok(printed_through);
    }
    for item in items
        .into_iter()
        .filter(|item| item.turn_id == Some(turn_id))
    {
        let (kind, text) = match item.data {
            ItemData::AssistantMessage { text } => ("assistant", text),
            ItemData::Reasoning { text } if thinking => ("reasoning", text),
            _ => continue,
        };
        if format == RunFormat::Json {
            println!(
                "{}",
                json!({
                    "type": kind,
                    "threadID": thread_id,
                    "turnID": turn_id,
                    "text": text,
                })
            );
        } else if kind == "reasoning" {
            println!("thinking:\n{text}");
        } else {
            println!("{text}");
        }
    }
    Ok(printed_through)
}

async fn settle_cli_subagents_client(
    client: &haarness_tui::api::ApiClient,
    thread_id: ThreadId,
    wait: bool,
    timeout_ms: Option<u64>,
    format: RunFormat,
    thinking: bool,
    printed_through: u64,
) -> Result<()> {
    let active = client.background_tasks(thread_id).await?;
    let parent_turn = client.active_turn(thread_id).await?;
    if !wait && active.is_empty() && parent_turn.is_none() {
        return Ok(());
    }
    if !wait {
        // Completion delivery removes the child from the task registry as
        // soon as its synthetic parent turn is admitted. That follow-up is
        // still background work from a one-shot client's point of view.
        let count = active.len().max(usize::from(parent_turn.is_some()));
        if format == RunFormat::Json {
            println!(
                "{}",
                json!({"type":"background_subagent", "status":"detached", "count":count})
            );
        } else {
            eprintln!(
                "{} background subagent{} continue{} on the Haarness server.",
                count,
                if count == 1 { "" } else { "s" },
                if count == 1 { "s" } else { "" }
            );
        }
        return Ok(());
    }

    let deadline = timeout_ms.map(|timeout| Instant::now() + Duration::from_millis(timeout));
    loop {
        let tasks = client.background_tasks(thread_id).await?;
        let parent_turn = client.active_turn(thread_id).await?;
        if tasks.is_empty() && parent_turn.is_none() {
            break;
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            anyhow::bail!(
                "background wait timed out; {} subagent{} continue{} on the Haarness server",
                tasks.len(),
                if tasks.len() == 1 { "" } else { "s" },
                if tasks.len() == 1 { "s" } else { "" }
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let mut failures = Vec::new();
    for item in client
        .items(thread_id)
        .await?
        .into_iter()
        .filter(|item| item.sequence > printed_through)
    {
        let turn_id = item.turn_id;
        match item.data {
            ItemData::AssistantMessage { text } => {
                if format == RunFormat::Json {
                    println!(
                        "{}",
                        json!({
                            "type":"assistant",
                            "threadID":thread_id,
                            "turnID":turn_id,
                            "text":text,
                        })
                    );
                } else {
                    println!("{text}");
                }
            }
            ItemData::Reasoning { text } if thinking => {
                if format == RunFormat::Json {
                    println!(
                        "{}",
                        json!({
                            "type":"reasoning",
                            "threadID":thread_id,
                            "turnID":turn_id,
                            "text":text,
                        })
                    );
                } else {
                    println!("thinking:\n{text}");
                }
            }
            ItemData::TaskState {
                child_thread_id,
                state,
                ..
            } if state == "failed" => {
                failures.push(format!("subagent {child_thread_id} ended as {state}"));
            }
            _ => {}
        }
    }
    if !failures.is_empty() {
        anyhow::bail!("background subagent failed: {}", failures.join("; "));
    }
    Ok(())
}

async fn settle_cli_workflows_client(
    client: &haarness_tui::api::ApiClient,
    thread_id: ThreadId,
    wait: bool,
    timeout_ms: Option<u64>,
    format: RunFormat,
) -> Result<()> {
    let active = client
        .workflow_runs(500)
        .await?
        .into_iter()
        .filter(|run| {
            run.definition.launch_thread_id == Some(thread_id)
                && run.status == haarness_protocol::WorkflowRunStatus::Running
        })
        .collect::<Vec<_>>();
    if active.is_empty() {
        return Ok(());
    }
    if !wait {
        if format == RunFormat::Json {
            println!(
                "{}",
                json!({"type":"background", "status":"detached", "count":active.len()})
            );
        } else {
            eprintln!(
                "{} background workflow{} continue{} on the Haarness server.",
                active.len(),
                if active.len() == 1 { "" } else { "s" },
                if active.len() == 1 { "s" } else { "" }
            );
        }
        return Ok(());
    }

    let deadline = timeout_ms.map(|timeout| Instant::now() + Duration::from_millis(timeout));
    let mut failures = Vec::new();
    for run in active {
        let current = loop {
            let current = client.workflow_run(run.id, None).await?;
            if current.status != haarness_protocol::WorkflowRunStatus::Running {
                break current;
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                anyhow::bail!(
                    "background wait timed out; workflow {} continues on the Haarness server (run {})",
                    current.workflow_name,
                    current.id
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        if format == RunFormat::Json {
            println!(
                "{}",
                json!({
                    "type":"workflow",
                    "runID":current.id,
                    "name":&current.workflow_name,
                    "status":current.status,
                    "result":&current.result,
                    "error":&current.error,
                })
            );
        } else {
            eprintln!(
                "Workflow {} finished as {:?}.",
                current.workflow_name, current.status
            );
            if let Some(result) = &current.result {
                match result {
                    serde_json::Value::String(text) if !text.trim().is_empty() => {
                        println!("{text}")
                    }
                    serde_json::Value::Null => {}
                    value => println!("{}", serde_json::to_string_pretty(value)?),
                }
            }
        }
        if current.status != haarness_protocol::WorkflowRunStatus::Completed {
            failures.push(
                current.error.unwrap_or_else(|| {
                    format!("workflow {} did not complete", current.workflow_name)
                }),
            );
        }
    }
    if !failures.is_empty() {
        anyhow::bail!("background workflow failed: {}", failures.join("; "));
    }
    Ok(())
}

#[cfg(test)]
async fn set_goal_for_run(
    runtime: &AgentRuntime,
    thread_id: ThreadId,
    objective: String,
) -> Result<()> {
    if runtime.active_turn(thread_id).await?.is_some() {
        anyhow::bail!(
            "cannot start a goal in session {thread_id} while its current turn is still running"
        );
    }
    runtime
        .goal_action(
            thread_id,
            GoalActionRequest::Set {
                goal: objective,
                token_budget: None,
            },
        )
        .await?;
    Ok(())
}

#[cfg(test)]
async fn select_run_thread(
    runtime: &AgentRuntime,
    requested_thread: Option<Uuid>,
    continue_last: bool,
    fork: bool,
    create: CreateThreadRequest,
) -> Result<Thread> {
    if fork && requested_thread.is_none() && !continue_last {
        anyhow::bail!("--fork requires --session/--thread or --continue");
    }

    let base = if let Some(id) = requested_thread {
        Some(runtime.get_thread(ThreadId(id)).await?)
    } else if continue_last {
        runtime
            .list_threads(false)
            .await?
            .into_iter()
            .find(|thread| thread.parent_id.is_none() && !thread.archived)
    } else {
        None
    };

    match base {
        Some(thread) if fork => Ok(runtime.fork_thread(thread.id, None, None).await?),
        Some(thread) => Ok(thread),
        None => Ok(runtime.create_thread(create).await?),
    }
}

const DEFAULT_GOAL_IDLE_TIMEOUT_MS: u64 = 45_000;

fn goal_idle_timeout_from(value: Option<&str>) -> Duration {
    let milliseconds = value
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_GOAL_IDLE_TIMEOUT_MS);
    Duration::from_millis(milliseconds)
}

fn goal_idle_timeout() -> Duration {
    let configured = [
        "HAARNESS_RUN_GOAL_IDLE_TIMEOUT_MS",
        "UPSTREAM_RUN_GOAL_IDLE_TIMEOUT_MS",
        "OPENCODE_RUN_GOAL_IDLE_TIMEOUT_MS",
    ]
    .into_iter()
    .find_map(|name| std::env::var(name).ok());
    goal_idle_timeout_from(configured.as_deref())
}

#[cfg(test)]
async fn wait_for_goal_with_timeout(
    runtime: &AgentRuntime,
    thread_id: ThreadId,
    subscription: EventSubscription,
    timeout: Duration,
) -> Result<()> {
    let mut receiver = subscription.receiver;
    let mut idle_since = None;
    loop {
        resolve_noninteractive_questions(runtime, thread_id)?;
        let state = runtime
            .goal_state(thread_id)
            .await?
            .context("goal was cleared before completion")?;
        if !state.status.is_active() {
            return finish_goal_run(&state);
        }

        if runtime.active_turn(thread_id).await?.is_some() {
            idle_since = None;
        } else {
            let since = idle_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= timeout {
                anyhow::bail!(
                    "goal watcher stopped after {}s without a goal update while idle",
                    timeout.as_secs_f64()
                );
            }
        }

        let poll = idle_since
            .map(|since| timeout.saturating_sub(since.elapsed()))
            .unwrap_or(Duration::from_millis(250))
            .min(Duration::from_millis(250));
        match tokio::time::timeout(poll, receiver.recv()).await {
            Ok(Ok(event))
                if event.thread_id == Some(thread_id) && event.event_type == "goal.updated" =>
            {
                let update: GoalUpdatedEvent =
                    serde_json::from_value(event.payload).context("decode goal.updated event")?;
                if !update.state.status.is_active() {
                    return finish_goal_run(&update.state);
                }
                if let Some(message) = update.message.filter(|message| !message.trim().is_empty()) {
                    eprintln!("{message}");
                }
                idle_since = None;
            }
            Ok(Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) | Err(_) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                anyhow::bail!("goal event stream closed before completion");
            }
        }
    }
}

#[cfg(test)]
fn resolve_noninteractive_questions(
    runtime: &AgentRuntime,
    root_thread_id: ThreadId,
) -> Result<()> {
    for question in runtime.pending_questions().into_iter().filter(|question| {
        question.thread_id == root_thread_id || question.root_thread_id == Some(root_thread_id)
    }) {
        let answer = if question.multiple && question.custom {
            // headless fallback: an empty multi-selection
            // lets goal-criteria negotiation accept the proposed set without
            // inventing a criterion from refusal prose.
            "[]".into()
        } else {
            question
                .choices
                .iter()
                .find(|choice| choice.trim().eq_ignore_ascii_case("cancel"))
                .cloned()
                .unwrap_or_else(|| {
                    "Question rejected: this non-interactive run cannot answer questions.".into()
                })
        };
        eprintln!(
            "Question requested by thread {}; applying the non-interactive fallback.",
            question.thread_id
        );
        runtime.reply_question(&question.question_id, QuestionReply { answer })?;
    }
    Ok(())
}

fn finish_goal_run(state: &GoalState) -> Result<()> {
    let reason = state
        .paused_reason
        .as_deref()
        .or(state.last_reason.as_deref());
    match state.status {
        GoalStatus::Complete => {
            eprintln!(
                "Goal complete ({} tokens, {} turns, {}s): {}",
                state.tokens_used, state.turns_used, state.time_used_seconds, state.goal
            );
            Ok(())
        }
        GoalStatus::Paused => anyhow::bail!(
            "goal paused before completion{}",
            reason
                .map(|reason| format!(": {reason}"))
                .unwrap_or_default()
        ),
        GoalStatus::Blocked => anyhow::bail!(
            "goal blocked{}",
            reason
                .map(|reason| format!(": {reason}"))
                .unwrap_or_default()
        ),
        GoalStatus::UsageLimited => anyhow::bail!("goal stopped at the provider usage limit"),
        GoalStatus::BudgetLimited => anyhow::bail!(
            "goal stopped at its token budget ({} / {} tokens)",
            state.tokens_used,
            state.token_budget.unwrap_or_default()
        ),
        GoalStatus::Cleared => anyhow::bail!("goal was cleared before completion"),
        GoalStatus::Active => anyhow::bail!("goal watcher stopped while the goal was active"),
    }
}

fn assistant_messages_for_turn(
    items: Vec<haarness_protocol::Item>,
    turn_id: TurnId,
) -> Vec<String> {
    items
        .into_iter()
        .filter(|item| item.turn_id == Some(turn_id))
        .filter_map(|item| match item.data {
            ItemData::AssistantMessage { text } => Some(text),
            _ => None,
        })
        .collect()
}

struct ComposedRuntimeSources {
    file_config: FileConfig,
    json_layers: jsonconfig::JsonLayers,
    skill_roots: Vec<(PathBuf, bool, Option<PathBuf>)>,
    workflow_catalog: WorkflowCatalog,
    project_context: String,
    runtime_config: haarness_core::RuntimeConfig,
}

fn compose_runtime_sources(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    yolo: bool,
    persisted_rules: Vec<haarness_core::PermissionRule>,
    reject_json_errors: bool,
) -> Result<ComposedRuntimeSources> {
    let _profile = haarness_profiles::configuration_lock(&paths.data)?;
    let mut file_config = load_config(paths, workspace, trust)?;
    let config_dir = paths
        .config
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let plugin_roots =
        plugins::all_enabled_roots(&paths.data, paths.config_dir(), workspace, trust);
    let mut json_layers = jsonconfig::load_layers(
        &config_dir,
        home.as_deref(),
        workspace,
        trust,
        &plugin_roots,
    );
    if reject_json_errors && !json_layers.errors.is_empty() {
        anyhow::bail!(json_layers.errors.join("; "));
    }
    file_config.mcp.extend(json_layers.mcp.clone());
    if let Some(overrides) = capability_state::load(paths, workspace, trust)?.get("mcp") {
        for (name, enabled) in overrides { if let Some(entry) = file_config.mcp.get_mut(name) { entry.enabled = *enabled; } }
    }

    let skill_roots = skill_roots(paths, workspace, trust, &file_config);
    // Remote skill caches are runtime data, so their slash commands are
    // discovered here rather than by the configuration layer.
    let remote_cache = remote_skill_cache(paths);
    for (root, _, _) in skill_roots
        .iter()
        .filter(|(root, _, _)| root.starts_with(&remote_cache))
    {
        jsonconfig::discover_skill_commands(&mut json_layers.commands, root, None);
    }

    let mut workflow_catalog = WorkflowCatalog::for_workspace(workspace).with_project_config(trust);
    for root in &plugin_roots {
        let fallback = root
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("plugin");
        workflow_catalog = workflow_catalog.with_plugin_mount(root, fallback);
    }
    let project_context = json_layers.instructions.clone().unwrap_or_default();
    let mut runtime_config = runtime_config(
        &file_config,
        json_layers.rules.clone(),
        persisted_rules,
        yolo,
    )?;
    runtime_config.disabled_agents = capability_state::disabled(paths, workspace, trust, "agent")?;
    runtime_config.disabled_agents.extend(json_layers.disabled_agents.clone());
    runtime_config.output_style = load_output_style(
        &config_dir,
        workspace,
        trust,
        file_config
            .defaults
            .as_ref()
            .and_then(|defaults| defaults.output_style.as_deref()),
    )?;
    runtime_config.base_rules.insert(
        0,
        haarness_core::PermissionRule {
            pattern: format!(
                "external_directory:{}/tool-output/*",
                paths
                    .data
                    .canonicalize()
                    .unwrap_or_else(|_| paths.data.clone())
                    .display()
            ),
            decision: haarness_core::PermissionDecision::Allow,
        },
    );
    runtime_config.read_roots = skill_roots
        .iter()
        .filter_map(|(path, _, _)| path.canonicalize().ok())
        .collect();
    runtime_config.read_roots.extend(
        plugins::all_enabled_roots(&paths.data, paths.config_dir(), workspace, trust)
            .into_iter()
            .filter_map(|path| path.canonicalize().ok()),
    );
    runtime_config.read_roots.sort();
    runtime_config.read_roots.dedup();
    runtime_config.commands.clone_from(&json_layers.commands);
    let disabled_skills = capability_state::disabled(paths, workspace, trust, "skill")?;
    runtime_config.commands.retain(|command| !disabled_skills.contains(&command.name));
    memory_settings::load(&management::memory_root_for(paths, workspace))?
        .apply(&mut runtime_config);
    runtime_config.custom_agents.clone_from(&json_layers.agents);
    Ok(ComposedRuntimeSources {
        file_config,
        json_layers,
        skill_roots,
        workflow_catalog,
        project_context,
        runtime_config,
    })
}

fn load_output_style(
    config_dir: &Path,
    workspace: &Path,
    trust: bool,
    selected: Option<&str>,
) -> Result<Option<haarness_core::OutputStyle>> {
    let Some(id) = selected.map(str::trim).filter(|id| !id.is_empty()) else {
        return Ok(None);
    };
    if id == "none" {
        return Ok(None);
    }
    if !id
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        anyhow::bail!("defaults.output_style must be a simple style name");
    }
    let mut candidates = vec![config_dir.join("output-styles").join(format!("{id}.md"))];
    if trust {
        candidates.push(
            workspace
                .join(".haarness/output-styles")
                .join(format!("{id}.md")),
        );
    }
    let instructions = candidates
        .into_iter()
        .rev()
        .find_map(|path| fs::read_to_string(path).ok())
        .or_else(|| {
            Some(match id {
                "concise" => "Lead with the outcome. Be concise and use only the formatting needed for clarity.",
                "explanatory" => "Explain the result in plain language, include the important reasoning, and define unfamiliar technical terms.",
                "formal" => "Use precise, professional prose with explicit assumptions, evidence, and remaining limitations.",
                "minimal" => "Return only the requested result and essential caveats. Avoid preamble, repetition, and optional detail.",
                _ => return None,
            }.to_owned())
        })
        .ok_or_else(|| anyhow::anyhow!(
            "unknown output style '{id}'; add {id}.md under output-styles or choose concise, explanatory, formal, or minimal"
        ))?;
    if instructions.trim().is_empty() {
        anyhow::bail!("output style '{id}' is empty");
    }
    Ok(Some(haarness_core::OutputStyle {
        id: id.to_owned(),
        instructions,
    }))
}

struct HotReloadCandidate {
    disabled_skills: Vec<String>,
    providers: Arc<dyn ProviderRegistry>,
    tools: Arc<dyn ToolRegistry>,
    project_context: String,
    runtime_config: haarness_core::RuntimeConfig,
    hooks: Option<Arc<dyn haarness_core::HookPort>>,
    skill_roots: Vec<(PathBuf, bool, Option<PathBuf>)>,
    workflow_catalog: Arc<WorkflowCatalog>,
    mcp: Arc<NativeMcpRegistry>,
    lsp: haarness_language::LspConfig,
    formatter: haarness_language::FormatterConfig,
    memory_context: String,
    notices: Vec<String>,
    watch: ReloadWatch,
}

/// Configuration-dependent sources the hot-reload fingerprint must watch in
/// addition to the fixed set: Claude Code roots once a compatibility toggle
/// is on, and files named by `instructions` entries.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct ReloadWatch {
    claude: ClaudeCodeCompatibility,
    instruction_files: Vec<PathBuf>,
}

impl ReloadWatch {
    fn from_layers(layers: &jsonconfig::JsonLayers) -> Self {
        Self {
            claude: claude_compat(&layers.native),
            instruction_files: layers.instruction_files.clone(),
        }
    }
}

struct HotReloadController {
    browser: Arc<haarness_server::browser::BrowserBroker>,
    initial_fingerprint: String,
    watch: parking_lot::RwLock<ReloadWatch>,
    paths: Paths,
    workspace: PathBuf,
    trust: bool,
    yolo: bool,
    store: Arc<SqliteStore>,
    runtime: Arc<AgentRuntime>,
    skills: Arc<FilesystemSkillRegistry>,
    mcp: Arc<NativeMcpRegistry>,
    workflows: Arc<WorkflowCoordinator>,
    management: Arc<management::ManagementService>,
    language: Arc<haarness_language::LanguageService>,
    scheduler: Scheduler,
}

impl HotReloadController {
    async fn candidate(&self) -> Result<HotReloadCandidate> {
        let persisted_rules = haarness_core::ThreadRepository::load_permission_rules(
            self.store.as_ref(),
            &self.workspace.display().to_string(),
        )
        .await
        .unwrap_or_default();
        let sources = compose_runtime_sources(
            &self.paths,
            &self.workspace,
            self.trust,
            self.yolo,
            persisted_rules,
            true,
        )?;
        let credentials = resolved_credentials(&read_auth(&self.paths)?);
        validate_provider_definitions(&sources.file_config)?;
        let (instances, disabled_providers) =
            provider_instances(&sources.file_config, &credentials);
        let providers: Arc<dyn ProviderRegistry> = Arc::new(
            NativeProviderRegistry::discover_with_providers_and_store(
                &credentials,
                instances,
                &disabled_providers,
                Some(Arc::new(CredentialStore::new(self.paths.auth.clone()))),
            )
            .await,
        );
        let candidate_mcp = mcp_from_config(&self.paths, &sources.file_config).await;
        let candidate_skills = FilesystemSkillRegistry::new_scoped(sources.skill_roots.clone());
        let disabled_skills = capability_state::disabled(&self.paths, &self.workspace, self.trust, "skill")?;
        candidate_skills.set_disabled(disabled_skills.clone());
        let skill_infos = candidate_skills.discover().await.unwrap_or_default();
        let tools: Arc<dyn ToolRegistry> = Arc::new(
            NativeToolRegistry::with_workflows_and_scheduler(
                self.workspace.clone(),
                Some(self.mcp.clone()),
                Some(self.skills.clone()),
                skill_infos,
                self.workflows.clone(),
                self.scheduler.clone(),
                self.trust,
            )
            .with_browser(self.browser.clone())
            .with_management(self.management.clone())
            .with_language(self.workspace.clone(), self.language.clone())
            .with_claude_instructions(claude_compat(&sources.file_config).instructions()),
        );
        let hooks = (!sources.json_layers.hooks.is_empty()).then(|| {
            Arc::new(haarness_server::ConfiguredHooks::new(
                sources.json_layers.hooks.clone(),
                self.workspace.clone(),
            )) as Arc<dyn haarness_core::HookPort>
        });
        let extensions = extensions::load(&self.paths, &self.workspace, self.trust).await?;
        let tools = extensions.wrap_tools(tools).await?;
        let hooks = if extensions.names().is_empty() {
            hooks
        } else {
            Some(extensions.wrap_hooks(hooks))
        };
        extensions
            .notify_config(&extensions::config_summary(
                &self.workspace,
                &sources.json_layers,
                &sources.runtime_config,
                &extensions,
            ))
            .await;
        let mut notices = sources.json_layers.notices.clone();
        notices.extend(config_specs::startup_notices(
            &self.paths,
            &self.workspace,
            self.trust,
        ));
        Ok(HotReloadCandidate {
            disabled_skills,
            providers,
            tools,
            project_context: sources.project_context,
            runtime_config: sources.runtime_config,
            hooks,
            skill_roots: sources.skill_roots,
            workflow_catalog: Arc::new(sources.workflow_catalog),
            mcp: candidate_mcp,
            lsp: sources.file_config.lsp.unwrap_or_default(),
            formatter: sources.file_config.formatter.unwrap_or_default(),
            memory_context: self.management.memory_index()?,
            watch: ReloadWatch::from_layers(&sources.json_layers),
            notices,
        })
    }

    fn apply(&self, candidate: &HotReloadCandidate) -> bool {
        let applied = self.apply_components(candidate);
        if applied {
            *self.watch.write() = candidate.watch.clone();
        }
        applied
    }

    fn apply_components(&self, candidate: &HotReloadCandidate) -> bool {
        let skills = self.skills.clone();
        let disabled_skills = candidate.disabled_skills.clone();
        let skill_roots = candidate.skill_roots.clone();
        let mcp = self.mcp.clone();
        let candidate_mcp = candidate.mcp.clone();
        let workflows = self.workflows.clone();
        let workflow_catalog = candidate.workflow_catalog.clone();
        let language = self.language.clone();
        let lsp = candidate.lsp.clone();
        let formatter = candidate.formatter.clone();
        let runtime = self.runtime.clone();
        let memory_context = candidate.memory_context.clone();
        self.runtime.replace_components_when_idle_with(
            candidate.providers.clone(),
            candidate.tools.clone(),
            candidate.project_context.clone(),
            candidate.runtime_config.clone(),
            candidate.hooks.clone(),
            move || {
                skills.replace_scoped_roots(skill_roots);
                skills.set_disabled(disabled_skills);
                mcp.replace_from(&candidate_mcp);
                workflows.replace_catalog(workflow_catalog);
                language.replace_config(lsp, formatter);
                runtime.set_memory_context(memory_context);
            },
        )
    }

    async fn run(self, shutdown: tokio_util::sync::CancellationToken) {
        let mut observed = self.initial_fingerprint.clone();
        let mut settle_since = None;
        let mut pending: Option<HotReloadCandidate> = None;
        let mut queued = false;
        let mut interval = tokio::time::interval(Duration::from_millis(250));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                _ = interval.tick() => {}
            }
            let fingerprint = match runtime_source_fingerprint(&self) {
                Ok(fingerprint) => fingerprint,
                Err(error) => {
                    tracing::warn!(%error, "hot reload source scan failed");
                    continue;
                }
            };
            if fingerprint != observed {
                observed = fingerprint;
                settle_since = Some(Instant::now());
                pending = None;
                queued = false;
                continue;
            }
            if pending.is_none()
                && settle_since.is_some_and(|since| since.elapsed() >= Duration::from_millis(200))
            {
                match self.candidate().await {
                    Ok(candidate) => pending = Some(candidate),
                    Err(error) => {
                        tracing::warn!(%error, "configuration hot reload rejected; retaining last-known-good generation");
                        self.runtime.events().publish(
                            "configuration.reload_failed",
                            None,
                            None,
                            None,
                            json!({"error": concise_reload_error(&error)}),
                        );
                        settle_since = None;
                    }
                }
            }
            let Some(candidate) = pending.as_ref() else {
                continue;
            };
            if runtime_source_fingerprint(&self).is_ok_and(|current| current != observed) {
                pending = None;
                settle_since = Some(Instant::now());
                continue;
            }
            if self.apply(candidate) {
                for notice in &candidate.notices {
                    tracing::info!("config: {notice}");
                }
                if let Some(hooks) = candidate.hooks.clone() {
                    let detail = json!({"source": "reload", "cwd": self.workspace});
                    tokio::spawn(async move {
                        hooks.config_change(&detail).await;
                    });
                }
                self.runtime.events().publish(
                    "configuration.reloaded",
                    None,
                    None,
                    None,
                    json!({"status":"applied"}),
                );
                self.runtime.events().publish(
                    "assets.updated",
                    None,
                    None,
                    None,
                    json!({"resource":"configuration","action":"reload"}),
                );
                pending = None;
                settle_since = None;
                queued = false;
            } else if !queued {
                self.runtime.events().publish(
                    "configuration.reload_queued",
                    None,
                    None,
                    None,
                    json!({"status":"waiting_for_idle"}),
                );
                queued = true;
            }
        }
    }
}

fn concise_reload_error(error: &anyhow::Error) -> String {
    let text = format!("{error:#}");
    text.chars().take(1_000).collect()
}

fn runtime_source_fingerprint(controller: &HotReloadController) -> Result<String> {
    let watch = controller.watch.read().clone();
    runtime_source_fingerprint_for(
        &controller.paths,
        &controller.workspace,
        controller.trust,
        &controller.management.memory_root(),
        &watch,
    )
}

fn runtime_source_fingerprint_for(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    memory_root: &Path,
    watch: &ReloadWatch,
) -> Result<String> {
    let config_dir = paths
        .config
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut roots = vec![
        paths.config.clone(),
        paths.auth.clone(),
        paths.skills.clone(),
        memory_root.to_path_buf(),
        paths.data.join("plugins/installed.json"),
        paths.data.join("plugins/local"),
        config_dir.join("haarness.json"),
        config_dir.join("haarness.jsonc"),
        config_dir.join("opencode.json"),
        config_dir.join("opencode.jsonc"),
        config_dir.join("AGENTS.md"),
        config_dir.join("commands"),
        config_dir.join("command"),
        config_dir.join("agents"),
        config_dir.join("output-styles"),
        config_dir.join("extensions.json"),
        config_dir.join("extensions"),
    ];
    roots.extend(capability_state::sources(paths, workspace, trust));
    for name in [
        "haarness.json",
        "haarness.jsonc",
        "opencode.json",
        "opencode.jsonc",
        "AGENTS.md",
    ] {
        roots.push(workspace.join(name));
    }
    for name in [".haarness", ".opencode", ".upstream", ".agents"] {
        roots.push(workspace.join(name));
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        roots.extend([
            home.join(".agents/skills"),
            home.join(".opencode/commands"),
            home.join(".opencode/agents"),
            home.join(".opencode/skills"),
            home.join(".upstream/commands"),
            home.join(".upstream/agents"),
            home.join(".upstream/skills"),
        ]);
    }
    roots.extend(plugins::all_enabled_roots(
        &paths.data,
        paths.config_dir(),
        workspace,
        trust,
    ));
    let extensions = extensions::sources(paths, workspace, trust)?;
    if extensions.enabled {
        for entry in extensions.entries.iter().filter(|entry| entry.enabled) {
            extensions::watch_sources(&entry.source, &mut roots)?;
        }
    }
    roots.push(config_dir.join("plugins/local"));
    roots.push(config_dir.join("agent"));
    roots.extend(watch.instruction_files.iter().cloned());
    let claude = &watch.claude;
    if claude.any() {
        let mut claude_roots = |base: &Path, project: bool| {
            if claude.instructions() {
                roots.push(base.join("CLAUDE.md"));
                if project {
                    for name in [
                        "AGENTS.local.md",
                        "CLAUDE.local.md",
                        "CONTEXT.md",
                        "CONTEXT.local.md",
                        ".claude/CLAUDE.md",
                        ".claude/CLAUDE.local.md",
                    ] {
                        roots.push(base.join(name));
                    }
                }
            }
            if claude.agents() {
                roots.push(base.join(".claude/agents"));
            }
            if claude.commands() {
                roots.push(base.join(".claude/commands"));
            }
            if claude.skills() {
                roots.push(base.join(".claude/skills"));
            }
        };
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            claude_roots(&home.join(".claude"), false);
        }
        for directory in layout::project_directories(workspace) {
            claude_roots(&directory, true);
        }
    }
    for directory in layout::project_directories(workspace) {
        for name in [
            "haarness.json",
            "haarness.jsonc",
            "upstream.json",
            "upstream.jsonc",
            "opencode.json",
            "opencode.jsonc",
            ".haarness",
            ".upstream",
            ".opencode",
            ".agents",
            "AGENTS.md",
        ] {
            roots.push(directory.join(name));
        }
    }
    roots.sort();
    roots.dedup();
    let mut hasher = Sha256::new();
    for root in roots {
        fingerprint_path(&root, 0, &mut hasher)?;
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn fingerprint_path(path: &Path, depth: usize, hasher: &mut Sha256) -> Result<()> {
    hasher.update(path.as_os_str().to_string_lossy().as_bytes());
    if depth > 24 {
        return Ok(());
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            hasher.update([0]);
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    hasher.update(metadata.len().to_le_bytes());
    if let Ok(modified) = metadata.modified()
        && let Ok(duration) = modified.duration_since(UNIX_EPOCH)
    {
        hasher.update(duration.as_secs().to_le_bytes());
        hasher.update(duration.subsec_nanos().to_le_bytes());
    }
    if metadata.file_type().is_symlink() {
        if let Ok(target) = fs::read_link(path) {
            hasher.update(target.as_os_str().to_string_lossy().as_bytes());
        }
        return Ok(());
    }
    if metadata.is_file() {
        let content_sensitive = matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("json" | "jsonc" | "toml" | "md" | "ts" | "js" | "mjs" | "mts")
        );
        if content_sensitive && metadata.len() <= 1024 * 1024 {
            hasher.update(fs::read(path)?);
        }
        return Ok(());
    }
    if !metadata.is_dir() {
        return Ok(());
    }
    let mut children = fs::read_dir(path)?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            !matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some(".git" | "node_modules" | "target" | "__pycache__" | ".DS_Store")
            )
        })
        .collect::<Vec<_>>();
    children.sort();
    for child in children {
        fingerprint_path(&child, depth + 1, hasher)?;
    }
    Ok(())
}

async fn application_at(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    yolo: bool,
    address: SocketAddr,
    recover: bool,
) -> Result<(TcpListener, ServerState, HotReloadController)> {
    let listener = TcpListener::bind(address).await?;
    let workspace_manager = Arc::new(paths.workspace_manager()?);
    let registered_workspace = workspace_manager
        .register_local(workspace)
        .map_err(anyhow::Error::new)?;
    if paths
        .workspace_id
        .as_deref()
        .is_some_and(|id| id != registered_workspace.id)
    {
        anyhow::bail!("workspace registry identity changed while composing the server");
    }
    let store = Arc::new(SqliteStore::open(&paths.database)?);
    let mut migration_notices = Vec::new();
    if let Err(error) = jsonconfig::migrate_toml(&paths.config, &mut migration_notices) {
        tracing::warn!(error = %error, "config.toml migration failed; keeping TOML");
    }
    let persisted_rules = haarness_core::ThreadRepository::load_permission_rules(
        store.as_ref(),
        &workspace.display().to_string(),
    )
    .await
    .unwrap_or_default();
    // Anchor the watcher before composing the first generation. A source edit
    // during provider/MCP discovery must differ from this baseline and queue a
    // follow-up reload instead of being mistaken for the initial state.
    let initial_watch = ReloadWatch::from_layers(&asset_layers(paths, workspace, trust));
    let initial_fingerprint = runtime_source_fingerprint_for(
        paths,
        workspace,
        trust,
        &management::memory_root_for(paths, workspace),
        &initial_watch,
    )
    .unwrap_or_default();
    let sources = compose_runtime_sources(paths, workspace, trust, yolo, persisted_rules, false)?;
    let ComposedRuntimeSources {
        file_config,
        json_layers,
        skill_roots,
        workflow_catalog,
        project_context,
        runtime_config,
    } = sources;
    for notice in migration_notices.iter().chain(&json_layers.notices) {
        tracing::info!("config: {notice}");
    }
    for error in &json_layers.errors {
        tracing::warn!("config: {error}");
    }
    for notice in config_specs::startup_notices(paths, workspace, trust) {
        tracing::warn!("config: {notice}");
    }
    let credentials = resolved_credentials(&read_auth(paths).unwrap_or_default());
    let (instances, disabled_providers) = provider_instances(&file_config, &credentials);
    let providers = Arc::new(
        NativeProviderRegistry::discover_with_providers_and_store(
            &credentials,
            instances,
            &disabled_providers,
            Some(Arc::new(CredentialStore::new(paths.auth.clone()))),
        )
        .await,
    );
    let mcp = mcp_from_config(paths, &file_config).await;
    let skills = Arc::new(FilesystemSkillRegistry::new_scoped(skill_roots));
    skills.set_disabled(capability_state::disabled(paths, workspace, trust, "skill")?);
    let events = EventBus::new(2048);
    let workflow_host = WorkflowRuntimeAdapter::new();
    let workflows = Arc::new(WorkflowCoordinator::new(
        store.clone(),
        Arc::new(workflow_catalog),
        WorkflowExecutor::new(ExecutorConfig::default()),
        workflow_host.clone(),
        events.clone(),
        workspace.to_path_buf(),
        trust,
    ));
    let skill_infos = skills.discover().await.unwrap_or_default();
    let management = Arc::new(management::ManagementService::new(
        paths.clone(),
        workspace.to_path_buf(),
        trust,
        skills.clone(),
        mcp.clone(),
    ));
    let language = Arc::new(haarness_language::LanguageService::new(
        workspace.to_path_buf(),
        file_config.lsp.clone().unwrap_or_default(),
        file_config.formatter.clone().unwrap_or_default(),
    ));
    let scheduler = Scheduler::discover(std::env::current_exe()?, paths.root.clone())?;
    let browser_global = paths.config_dir().to_path_buf();
    let browser_workspace = workspace.to_path_buf();
    let browser = haarness_server::browser::BrowserBroker::with_policy(Arc::new(move || {
        browser_settings::load(trust, &browser_global, &browser_workspace).map_err(|error| {
            haarness_core::CoreError::Tool(format!("Cannot read browser settings: {error}"))
        })
    }));
    let tools: Arc<dyn ToolRegistry> = Arc::new(
        NativeToolRegistry::with_workflows_and_scheduler(
            workspace.to_path_buf(),
            Some(mcp.clone()),
            Some(skills.clone()),
            skill_infos,
            workflows.clone(),
            scheduler.clone(),
            trust,
        )
        .with_browser(browser.clone())
        .with_management(management.clone())
        .with_language(workspace.to_path_buf(), language.clone())
        .with_claude_instructions(claude_compat(&file_config).instructions()),
    );
    let extensions = extensions::load(paths, workspace, trust).await?;
    let tools = extensions.wrap_tools(tools).await?;
    let extension_config =
        extensions::config_summary(workspace, &json_layers, &runtime_config, &extensions);
    let mut runtime = AgentRuntime::with_config(
        store.clone(),
        providers,
        tools,
        events,
        workspace.to_path_buf(),
        project_context,
        runtime_config,
    );
    let hooks = (!json_layers.hooks.is_empty()).then(|| {
        Arc::new(haarness_server::ConfiguredHooks::new(
            json_layers.hooks.clone(),
            workspace.to_path_buf(),
        )) as Arc<dyn haarness_core::HookPort>
    });
    let hooks = if extensions.names().is_empty() {
        hooks
    } else {
        Some(extensions.wrap_hooks(hooks))
    };
    if let Some(hooks) = hooks {
        runtime = runtime.with_hooks(hooks);
    }
    extensions.notify_config(&extension_config).await;
    let snapshot_config = file_config.snapshots.clone().unwrap_or_default();
    let project_is_git = workspace_manager
        .project_for_workspace(&registered_workspace.id)?
        .git;
    if automatic_snapshots_enabled(file_config.snapshots.as_ref(), project_is_git) {
        match haarness_store::SqliteSnapshotStore::open(&paths.database, workspace.to_path_buf()) {
            Ok(snapshots) => {
                let snapshots = Arc::new(snapshots);
                spawn_snapshot_retention(Arc::clone(&snapshots), &snapshot_config);
                runtime = runtime.with_snapshots(snapshots);
            }
            Err(error) => eprintln!("Snapshots disabled: {error}"),
        }
    }
    let common = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output();
    if let Ok(common) = common
        && common.status.success()
    {
        let identity = PathBuf::from(String::from_utf8(common.stdout)?.trim()).canonicalize()?;
        runtime = runtime.with_repository_approvals(Arc::new(
            haarness_store::RepositoryApprovals::open(
                &paths.data.join("repository-approvals.sqlite3"),
                &identity,
            )?,
        ))?;
    }
    let runtime = Arc::new(runtime);
    management.attach_runtime(&runtime);
    workflow_host.attach(&runtime);
    if recover {
        let recovered = runtime.recover().await?;
        if recovered > 0 {
            eprintln!(
                "Recovered {recovered} interrupted turn(s); prior work is preserved and tools were not replayed."
            );
        }
        let recovered_workflows = workflows.recover().await?;
        if recovered_workflows > 0 {
            eprintln!(
                "Recovered {recovered_workflows} interrupted workflow(s); use /workflow to inspect or resume them."
            );
        }
    }
    // Only the long-lived server owns timers and recovery. One-shot CLI
    // compositions may share the same database while a server is alive; they
    // must never arm duplicate loop timers or "recover" work owned by it.
    if recover {
        spawn_loop_driver(runtime.clone());
    }
    let hot_reload = HotReloadController {
        browser: browser.clone(),
        initial_fingerprint,
        watch: parking_lot::RwLock::new(initial_watch),
        paths: paths.clone(),
        workspace: workspace.to_path_buf(),
        trust,
        yolo,
        store,
        runtime: runtime.clone(),
        skills: skills.clone(),
        mcp: mcp.clone(),
        workflows: workflows.clone(),
        management: management.clone(),
        language: language.clone(),
        scheduler,
    };
    Ok((
        listener,
        ServerState {
            browser: Some(browser),
            management: Some(management),
            runtime,
            mcp,
            skills,
            plugins: Some(Arc::new(plugins::PluginService::new(
                paths.data.clone(),
                paths.config_dir().to_path_buf(),
                workspace.to_path_buf(),
                trust,
            ))),
            workflows: Some(workflows),
            workspaces: Some(workspace_manager),
            workspace_id: Some(registered_workspace.id),
            workspace: workspace.to_path_buf(),
            project_trusted: trust,
            language: Some(language),
        },
        hot_reload,
    ))
}

/// Test helper proving trusted MCP configuration composes identically to the
/// registry owned by the persistent server.
#[cfg(test)]
async fn configured_mcp(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
) -> Result<Arc<NativeMcpRegistry>> {
    Ok(mcp_from_config(paths, &load_config(paths, workspace, trust)?).await)
}

async fn mcp_from_config(paths: &Paths, config: &FileConfig) -> Arc<NativeMcpRegistry> {
    let registry = Arc::new(NativeMcpRegistry::with_auth_dir(
        paths.data.join("mcp-oauth"),
    ));
    for (name, server) in &config.mcp {
        if let Err(error) = registry.configure(name, server.clone()).await {
            eprintln!("MCP {name}: {error}");
        }
    }
    registry
}

async fn auth_command(paths: &Paths, command: AuthCommand) -> Result<()> {
    let store = CredentialStore::new(paths.auth.clone());
    match command {
        AuthCommand::Set { provider, key } => {
            let key = key.map_or_else(
                || rpassword::prompt_password(format!("{provider} API key: ")),
                Ok,
            )?;
            store.set(&provider, &Credential::api(key))?;
            println!("Stored {provider} credential in owner-only auth.json");
        }
        AuthCommand::Login { provider, method } => {
            let provider = provider.trim_end_matches('/').to_ascii_lowercase();
            if haarness_protocol::provider_storage_id(&provider) == "haarness" {
                return account::login(&account::LoginArgs {
                    device: method == Some(AuthLoginMethod::Headless),
                    no_browser: false,
                    json: false,
                })
                .await;
            }
            let method = method.unwrap_or(
                if matches!(provider.as_str(), "openai" | "xai" | "github-copilot") {
                    AuthLoginMethod::Browser
                } else {
                    AuthLoginMethod::Api
                },
            );
            let credential = match method {
                AuthLoginMethod::Api => {
                    let key = rpassword::prompt_password(format!("{provider} API key: "))?;
                    Credential::api(key)
                }
                AuthLoginMethod::Browser if provider == "openai" => {
                    let login = OpenAiOAuthClient::default().start_browser_login().await?;
                    println!("Open this URL to connect ChatGPT:\n{}", login.url);
                    if let Err(error) = open_browser(login.url.as_str()) {
                        eprintln!("Could not open a browser automatically: {error}");
                    }
                    login.finish().await?
                }
                AuthLoginMethod::Headless if provider == "openai" => {
                    let login = OpenAiOAuthClient::default().start_device_login().await?;
                    println!(
                        "Open {} and enter code {}",
                        login.verification_url, login.user_code
                    );
                    login.finish().await?
                }
                AuthLoginMethod::Browser if provider == "xai" => {
                    let login = XaiOAuthClient::default().start_device_login().await?;
                    println!(
                        "Open {} and enter code {}",
                        login.instructions_url, login.user_code
                    );
                    if let Err(error) = open_browser(&login.verification_url) {
                        eprintln!("Could not open a browser automatically: {error}");
                    }
                    login.finish().await?
                }
                AuthLoginMethod::Headless if provider == "xai" => {
                    let login = XaiOAuthClient::default().start_device_login().await?;
                    println!(
                        "Open {} and enter code {}",
                        login.instructions_url, login.user_code
                    );
                    login.finish().await?
                }
                AuthLoginMethod::Browser if provider == "github-copilot" => {
                    let login = CopilotOAuthClient::default().start_device_login().await?;
                    println!(
                        "Open {} and enter code {}",
                        login.verification_url, login.user_code
                    );
                    if let Err(error) = open_browser(&login.verification_url) {
                        eprintln!("Could not open a browser automatically: {error}");
                    }
                    login.finish().await?
                }
                AuthLoginMethod::Headless if provider == "github-copilot" => {
                    let login = CopilotOAuthClient::default().start_device_login().await?;
                    println!(
                        "Open {} and enter code {}",
                        login.verification_url, login.user_code
                    );
                    login.finish().await?
                }
                AuthLoginMethod::Browser | AuthLoginMethod::Headless => {
                    anyhow::bail!(
                        "{provider} has no built-in subscription OAuth method; use --method api"
                    );
                }
            };
            store.set(&provider, &credential)?;
            println!("Connected {provider} ({})", credential.kind());
        }
        AuthCommand::Logout { provider } => {
            if matches!(
                provider.to_ascii_lowercase().as_str(),
                "haarness" | "haarness-cloud"
            ) {
                return account::logout().await;
            }
            if store.remove(&provider)? {
                println!("Removed {provider} credential");
            } else {
                println!("No stored {provider} credential");
            }
        }
        AuthCommand::List => {
            for (provider, credential) in store.all()? {
                println!("{provider}\t{}", credential.kind());
            }
        }
    }
    Ok(())
}

fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let status = ProcessCommand::new("open").arg(url).status()?;
    #[cfg(target_os = "linux")]
    let status = ProcessCommand::new("xdg-open").arg(url).status()?;
    #[cfg(target_os = "windows")]
    let status = ProcessCommand::new("cmd")
        .args(["/C", "start", "", url])
        .status()?;
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    anyhow::bail!("automatic browser opening is not supported on this platform");
    if !status.success() {
        anyhow::bail!("browser command exited with {status}");
    }
    Ok(())
}
async fn mcp_command(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    yolo: bool,
    command: McpCommand,
) -> Result<()> {
    let discovery = ensure_workspace_server(paths, workspace, trust, yolo).await?;
    let client = haarness_tui::api::ApiClient::new(&discovery.url, &discovery.token)?;
    match command {
        McpCommand::List => {
            let servers = client.mcp().await?;
            if servers.is_empty() {
                println!("No MCP servers configured.");
            }
            for server in servers {
                println!(
                    "{}\t{}\t{}\t{}",
                    server.name,
                    server.transport,
                    server.status,
                    server.detail.as_deref().unwrap_or("")
                );
            }
        }
        McpCommand::Reconnect { name } => {
            let server = client.reconnect_mcp(&name).await?;
            println!("{}: {}", server.name, server.status);
        }
        McpCommand::Disconnect { name } => {
            let server = client.disconnect_mcp(&name).await?;
            println!("{}: {}", server.name, server.status);
        }
        McpCommand::Tools { name } => {
            let tools = client.mcp_tools(&name).await?;
            if tools.is_empty() {
                println!("No tools advertised by '{name}'.");
            }
            for tool in tools {
                println!(
                    "{}\t{}",
                    tool["name"].as_str().unwrap_or("<unnamed>"),
                    tool["description"].as_str().unwrap_or("")
                );
            }
        }
        McpCommand::Call {
            name,
            tool,
            arguments,
        } => {
            let arguments = mcp_arguments(&arguments)?;
            let output = client.call_mcp_tool(&name, &tool, &arguments).await?;
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        McpCommand::Resources { name } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&client.mcp_resources(&name).await?)?
            );
        }
        McpCommand::ResourceTemplates { name } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&client.mcp_resource_templates(&name).await?)?
            );
        }
        McpCommand::Read { name, uri } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&client.read_mcp_resource(&name, &uri).await?)?
            );
        }
        McpCommand::Prompts { name } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&client.mcp_prompts(&name).await?)?
            );
        }
        McpCommand::Prompt {
            name,
            prompt,
            arguments,
        } => {
            let arguments = mcp_arguments(&arguments)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &client.get_mcp_prompt(&name, &prompt, &arguments).await?
                )?
            );
        }
        McpCommand::Auth {
            name,
            project,
            no_browser,
            callback_port,
        } => {
            let target = mcp_entry_target(paths, workspace, project, &name);
            let entry = read_mcp_entry(&target, &name)?;
            let registry = oauth_registry(paths, &name, &entry, true).await?;
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, callback_port))
                .await
                .context("cannot bind the MCP OAuth loopback callback")?;
            let address = listener.local_addr()?;
            let redirect_uri = format!("http://{address}/mcp/oauth/callback");
            let session = registry.begin_oauth(&name, &redirect_uri).await?;
            println!("Authorize MCP server '{name}' in your browser:");
            println!("{}", session.authorization_url);
            if !no_browser && let Err(error) = open_browser(&session.authorization_url) {
                eprintln!("Could not open a browser automatically: {error}");
            }
            let callback_url = receive_oauth_callback(&listener, &redirect_uri).await?;
            session.complete(&callback_url).await?;
            let server = client.configure_mcp(&name, &entry).await?;
            println!("{}: {}", server.name, server.status);
        }
        McpCommand::Logout { name, project } => {
            let target = mcp_entry_target(paths, workspace, project, &name);
            let entry = read_mcp_entry(&target, &name)?;
            let registry = oauth_registry(paths, &name, &entry, false).await?;
            registry.clear_oauth(&name).await?;
            let server = client.configure_mcp(&name, &entry).await?;
            println!(
                "Cleared OAuth credentials for '{}': {}",
                server.name, server.status
            );
        }
        McpCommand::Get {
            name,
            project,
            show_secrets,
        } => {
            let target = mcp_entry_target(paths, workspace, project, &name);
            let entry = read_mcp_entry(&target, &name)?;
            let config = mcp_config_for_display(&entry, show_secrets)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "name": name,
                    "scope": if project { "project" } else { "global" },
                    "file": target,
                    "config": config,
                }))?
            );
        }
        McpCommand::Add {
            name,
            command,
            args,
            env,
            url,
            headers,
            oauth,
            no_oauth,
            oauth_client_id,
            oauth_scopes,
            disabled,
            project,
        } => {
            validate_mcp_name(&name)?;
            let env = parse_mcp_key_values(&env, "environment variable")?;
            let headers = parse_mcp_key_values(&headers, "header")?;
            let entry = match (&command, &url) {
                (Some(command), None) => {
                    if !headers.is_empty()
                        || oauth
                        || no_oauth
                        || oauth_client_id.is_some()
                        || !oauth_scopes.is_empty()
                    {
                        anyhow::bail!("remote headers and OAuth options are only valid with --url");
                    }
                    McpConfigureRequest {
                        transport: "stdio".into(),
                        command: Some(command.clone()),
                        args: args.clone(),
                        env,
                        cwd: None,
                        url: None,
                        headers: BTreeMap::new(),
                        headers_helper: None,
                        timeout_ms: None,
                        oauth: None,
                        oauth_client_id: None,
                        oauth_scopes: Vec::new(),
                        enabled: !disabled,
                    }
                }
                (None, Some(url)) => {
                    if !args.is_empty() || !env.is_empty() {
                        anyhow::bail!("--arg and --env are only valid with --command");
                    }
                    McpConfigureRequest {
                        transport: "streamable_http".into(),
                        command: None,
                        args: Vec::new(),
                        env: BTreeMap::new(),
                        cwd: None,
                        url: Some(url.clone()),
                        headers,
                        headers_helper: None,
                        timeout_ms: None,
                        oauth: if oauth {
                            Some(true)
                        } else if no_oauth {
                            Some(false)
                        } else {
                            None
                        },
                        oauth_client_id: oauth_client_id.clone(),
                        oauth_scopes: oauth_scopes.clone(),
                        enabled: !disabled,
                    }
                }
                _ => anyhow::bail!("provide exactly one of --command or --url"),
            };
            let target = mcp_entry_target(paths, workspace, project, &name);
            write_mcp_entry(&target, &name, Some(&entry))?;
            let server = client.configure_mcp(&name, &entry).await?;
            println!(
                "Added MCP server '{name}' to {} ({}).",
                target.display(),
                server.status
            );
            if project && !trust {
                println!("Note: project configuration is disabled for this invocation.");
            }
        }
        McpCommand::Enable { name, project } => {
            let entry = set_mcp_enabled(paths, workspace, &name, project, true)?;
            let server = client.configure_mcp(&name, &entry).await?;
            println!("{}: {}", server.name, server.status);
        }
        McpCommand::Disable { name, project } => {
            let entry = set_mcp_enabled(paths, workspace, &name, project, false)?;
            let server = client.configure_mcp(&name, &entry).await?;
            println!("{}: {}", server.name, server.status);
        }
        McpCommand::Remove { name, project } => {
            let target = mcp_entry_target(paths, workspace, project, &name);
            let mut entry = read_mcp_entry(&target, &name)?;
            entry.enabled = false;
            client.configure_mcp(&name, &entry).await?;
            write_mcp_entry(&target, &name, None)?;
            println!("Removed MCP server '{name}' from {}.", target.display());
        }
    }
    Ok(())
}

async fn oauth_registry(
    paths: &Paths,
    name: &str,
    entry: &McpConfigureRequest,
    probe: bool,
) -> Result<NativeMcpRegistry> {
    let registry = NativeMcpRegistry::with_auth_dir(paths.data.join("mcp-oauth"));
    let mut staged = entry.clone();
    staged.enabled = probe;
    registry.configure(name, staged).await?;
    Ok(registry)
}

fn mcp_arguments(arguments: &str) -> Result<serde_json::Value> {
    let arguments: serde_json::Value =
        serde_json::from_str(arguments).context("--arguments must be valid JSON")?;
    if !arguments.is_object() {
        anyhow::bail!("--arguments must be a JSON object");
    }
    Ok(arguments)
}

async fn receive_oauth_callback(listener: &TcpListener, redirect_uri: &str) -> Result<String> {
    const MAX_REQUEST_BYTES: usize = 32 * 1024;
    let (mut stream, peer) = tokio::time::timeout(Duration::from_secs(300), listener.accept())
        .await
        .context("timed out waiting for the MCP OAuth callback")??;
    if !peer.ip().is_loopback() {
        anyhow::bail!("rejected a non-loopback MCP OAuth callback");
    }
    let mut request = Vec::new();
    let mut chunk = [0_u8; 2048];
    loop {
        let count = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk))
            .await
            .context("timed out reading the MCP OAuth callback")??;
        if count == 0 {
            anyhow::bail!("MCP OAuth callback closed before sending a request");
        }
        request.extend_from_slice(&chunk[..count]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if request.len() > MAX_REQUEST_BYTES {
            anyhow::bail!("MCP OAuth callback request exceeded 32 KiB");
        }
    }
    let request = std::str::from_utf8(&request).context("MCP OAuth callback was not UTF-8")?;
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .context("MCP OAuth callback request line was invalid")?;
    if !target.starts_with("/mcp/oauth/callback?") {
        anyhow::bail!("MCP OAuth callback used an unexpected path");
    }
    let callback_url = format!("{redirect_uri}{}", &target["/mcp/oauth/callback".len()..]);
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: 60\r\nConnection: close\r\n\r\nAuthorization received. You may return to the Haarness CLI.\n",
        )
        .await?;
    Ok(callback_url)
}

fn validate_mcp_name(name: &str) -> Result<()> {
    if name.trim().is_empty() || name.chars().any(char::is_control) {
        anyhow::bail!("MCP server name must be non-empty and contain no control characters");
    }
    Ok(())
}

fn parse_mcp_key_values(values: &[String], label: &str) -> Result<BTreeMap<String, String>> {
    let mut parsed = BTreeMap::new();
    for value in values {
        let (key, value) = value
            .split_once('=')
            .with_context(|| format!("invalid {label} '{value}'; expected KEY=VALUE"))?;
        if key.trim().is_empty() || key.chars().any(char::is_control) {
            anyhow::bail!("invalid {label} name '{key}'");
        }
        if parsed.insert(key.to_owned(), value.to_owned()).is_some() {
            anyhow::bail!("duplicate {label} '{key}'");
        }
    }
    Ok(parsed)
}

fn set_mcp_enabled(
    paths: &Paths,
    workspace: &Path,
    name: &str,
    project: bool,
    enabled: bool,
) -> Result<McpConfigureRequest> {
    let target = mcp_entry_target(paths, workspace, project, name);
    let mut entry = read_mcp_entry(&target, name)?;
    entry.enabled = enabled;
    write_mcp_entry(&target, name, Some(&entry))?;
    println!(
        "{} MCP server '{name}' in {}.",
        if enabled { "Enabled" } else { "Disabled" },
        target.display()
    );
    Ok(entry)
}

fn read_mcp_entry(path: &Path, name: &str) -> Result<McpConfigureRequest> {
    if path.extension().is_some_and(|value| value != "toml") {
        return jsonconfig::read_json(path)
            .map_err(anyhow::Error::msg)?
            .and_then(|root| root.get("mcp")?.get(name).and_then(jsonconfig::mcp_entry))
            .with_context(|| format!("no MCP server named '{name}' in {}", path.display()));
    }
    let root: toml::Value = toml::from_str(
        &fs::read_to_string(path).with_context(|| format!("read MCP config {}", path.display()))?,
    )?;
    let value = root
        .get("mcp")
        .and_then(toml::Value::as_table)
        .and_then(|mcp| mcp.get(name))
        .cloned()
        .with_context(|| format!("no MCP server named '{name}' in {}", path.display()))?;
    value
        .try_into()
        .with_context(|| format!("invalid MCP server '{name}' in {}", path.display()))
}

fn mcp_config_for_display(
    entry: &McpConfigureRequest,
    show_secrets: bool,
) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(entry)?;
    if !show_secrets {
        for field in ["env", "headers"] {
            if let Some(values) = value
                .get_mut(field)
                .and_then(serde_json::Value::as_object_mut)
            {
                for value in values.values_mut() {
                    *value = serde_json::Value::String("<redacted>".into());
                }
            }
        }
        if value.get("headersHelper").is_some() {
            value["headersHelper"] = serde_json::Value::String("<redacted>".into());
        }
    }
    Ok(value)
}

fn scoped_config_files(paths: &Paths, workspace: &Path, project: bool) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if project {
        for directory in layout::project_directories(workspace) {
            files.push(directory.join(".haarness/cloud/haarness.json"));
            files.push(directory.join(".haarness/config.toml"));
            for root in [
                directory.clone(),
                directory.join(".opencode"),
                directory.join(".upstream"),
                directory.join(".haarness"),
            ] {
                for name in [
                    "opencode.jsonc",
                    "opencode.json",
                    "upstream.jsonc",
                    "upstream.json",
                    "haarness.jsonc",
                    "haarness.json",
                ] {
                    files.push(root.join(name));
                }
            }
        }
    } else {
        files.push(paths.config.clone());
        for name in [
            "opencode.jsonc",
            "opencode.json",
            "haarness.jsonc",
            "haarness.json",
        ] {
            files.push(paths.config_dir().join(name));
        }
    }
    files
}
fn scoped_config_target(paths: &Paths, workspace: &Path, project: bool) -> PathBuf {
    scoped_config_files(paths, workspace, project)
        .into_iter()
        .rev()
        .find(|path| {
            path.exists()
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("haarness."))
        })
        .or_else(|| {
            let path = if project {
                workspace.join(".haarness/config.toml")
            } else {
                paths.config.clone()
            };
            path.exists().then_some(path)
        })
        .unwrap_or_else(|| {
            if project {
                workspace.join("haarness.json")
            } else {
                paths.config_dir().join("haarness.json")
            }
        })
}
fn mcp_entry_target(paths: &Paths, workspace: &Path, project: bool, name: &str) -> PathBuf {
    scoped_config_files(paths, workspace, project)
        .into_iter()
        .rev()
        .find(|path| read_mcp_entry(path, name).is_ok())
        .unwrap_or_else(|| scoped_config_target(paths, workspace, project))
}

fn mcp_config_target(paths: &Paths, workspace: &Path, project: bool) -> PathBuf {
    scoped_config_target(paths, workspace, project)
}

/// Upsert or delete `[mcp.<name>]` in a TOML config file. TOML round-trips
/// through `toml::Value`, so comments in the file are not preserved — noted
/// in the CLI docs; JSON configs remain untouched.
fn write_mcp_entry(path: &Path, name: &str, entry: Option<&McpConfigureRequest>) -> Result<()> {
    if path.extension().is_some_and(|value| value != "toml") {
        return jsonconfig::write_entry(
            path,
            "mcp",
            name,
            entry.map(serde_json::to_value).transpose()?,
        );
    }
    let mut root: toml::Value = if path.exists() {
        toml::from_str(&fs::read_to_string(path)?)?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };
    let table = root.as_table_mut().context("config root must be a table")?;
    let mcp = table
        .entry("mcp")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .context("[mcp] must be a table")?;
    match entry {
        Some(entry) => {
            let mut server = toml::map::Map::new();
            server.insert(
                "transport".into(),
                toml::Value::String(entry.transport.clone()),
            );
            if let Some(command) = &entry.command {
                server.insert("command".into(), toml::Value::String(command.clone()));
            }
            if !entry.args.is_empty() {
                server.insert(
                    "args".into(),
                    toml::Value::Array(
                        entry
                            .args
                            .iter()
                            .map(|arg| toml::Value::String(arg.clone()))
                            .collect(),
                    ),
                );
            }
            if !entry.env.is_empty() {
                server.insert(
                    "env".into(),
                    toml::Value::Table(
                        entry
                            .env
                            .iter()
                            .map(|(key, value)| (key.clone(), toml::Value::String(value.clone())))
                            .collect(),
                    ),
                );
            }
            if let Some(cwd) = &entry.cwd {
                server.insert("cwd".into(), toml::Value::String(cwd.clone()));
            }
            if let Some(url) = &entry.url {
                server.insert("url".into(), toml::Value::String(url.clone()));
            }
            if !entry.headers.is_empty() {
                server.insert(
                    "headers".into(),
                    toml::Value::Table(
                        entry
                            .headers
                            .iter()
                            .map(|(key, value)| (key.clone(), toml::Value::String(value.clone())))
                            .collect(),
                    ),
                );
            }
            if let Some(helper) = &entry.headers_helper {
                server.insert("headersHelper".into(), toml::Value::String(helper.clone()));
            }
            if let Some(timeout) = entry.timeout_ms {
                server.insert(
                    "timeoutMs".into(),
                    toml::Value::Integer(i64::try_from(timeout).unwrap_or(i64::MAX)),
                );
            }
            if let Some(oauth) = entry.oauth {
                server.insert("oauth".into(), toml::Value::Boolean(oauth));
            }
            if let Some(client_id) = &entry.oauth_client_id {
                server.insert(
                    "oauthClientId".into(),
                    toml::Value::String(client_id.clone()),
                );
            }
            if !entry.oauth_scopes.is_empty() {
                server.insert(
                    "oauthScopes".into(),
                    toml::Value::Array(
                        entry
                            .oauth_scopes
                            .iter()
                            .map(|scope| toml::Value::String(scope.clone()))
                            .collect(),
                    ),
                );
            }
            server.insert("enabled".into(), toml::Value::Boolean(entry.enabled));
            mcp.insert(name.to_owned(), toml::Value::Table(server));
        }
        None => {
            if mcp.remove(name).is_none() {
                anyhow::bail!("no MCP server named '{name}' in {}", path.display());
            }
        }
    }
    if mcp.is_empty() {
        table.remove("mcp");
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, toml::to_string_pretty(&root)?)?;
    Ok(())
}

fn provider_config_target(paths: &Paths, workspace: &Path, project: bool) -> PathBuf {
    scoped_config_target(paths, workspace, project)
}

/// Upsert or delete `[providers.<id>]`. Like MCP management, this intentionally
/// round-trips TOML values and therefore does not preserve comments.
fn write_provider_entry(path: &Path, id: &str, entry: Option<&ProviderSection>) -> Result<()> {
    if path.extension().is_some_and(|value| value != "toml") {
        return jsonconfig::write_entry(
            path,
            "providers",
            id,
            entry.map(serde_json::to_value).transpose()?,
        );
    }
    let mut root: toml::Value = if path.exists() {
        toml::from_str(&fs::read_to_string(path)?)?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };
    let table = root.as_table_mut().context("config root must be a table")?;
    let providers = table
        .entry("providers")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .context("[providers] must be a table")?;
    match entry {
        Some(entry) => {
            let mut provider = toml::map::Map::new();
            provider.insert(
                "protocol".into(),
                toml::Value::String(entry.protocol.clone()),
            );
            if let Some(name) = &entry.name {
                provider.insert("name".into(), toml::Value::String(name.clone()));
            }
            if let Some(url) = &entry.base_url {
                provider.insert("base_url".into(), toml::Value::String(url.clone()));
            }
            if let Some(variable) = &entry.api_key_env {
                provider.insert("api_key_env".into(), toml::Value::String(variable.clone()));
            }
            if !entry.headers.is_empty() {
                provider.insert(
                    "headers".into(),
                    toml::Value::Table(
                        entry
                            .headers
                            .iter()
                            .map(|(name, value)| (name.clone(), toml::Value::String(value.clone())))
                            .collect(),
                    ),
                );
            }
            if !entry.models.is_empty() {
                provider.insert(
                    "models".into(),
                    toml::Value::Array(
                        entry
                            .models
                            .iter()
                            .map(|model| toml::Value::String(model.clone()))
                            .collect(),
                    ),
                );
            }
            provider.insert(
                "requires_auth".into(),
                toml::Value::Boolean(entry.requires_auth),
            );
            if let Some(integration) = &entry.integration {
                provider.insert("integration".into(), toml::Value::try_from(integration)?);
            }
            providers.insert(id.to_owned(), toml::Value::Table(provider));
        }
        None => {
            if providers.remove(id).is_none() {
                anyhow::bail!("no custom provider named '{id}' in {}", path.display());
            }
        }
    }
    if providers.is_empty() {
        table.remove("providers");
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, toml::to_string_pretty(&root)?)?;
    Ok(())
}

fn doctor(paths: &Paths) -> Result<()> {
    let report = haarness_opentui::doctor()?;
    println!("Haarness {}", env!("CARGO_PKG_VERSION"));
    println!("OpenTUI {} ({})", report.version, report.target);
    println!("native: {}", report.path.display());
    println!("sha256: {}", report.sha256);
    println!("symbols: {}", report.symbols.join(", "));
    let ripgrep = haarness_ripgrep::binary_path()?;
    let output = std::process::Command::new(&ripgrep)
        .arg("--version")
        .output()?;
    anyhow::ensure!(output.status.success(), "bundled ripgrep failed to execute");
    println!(
        "ripgrep: {} ({})",
        haarness_ripgrep::VERSION,
        haarness_ripgrep::TARGET
    );
    println!("rg: {}", ripgrep.display());
    println!("rg sha256: {}", haarness_ripgrep::SHA256);
    println!("data: {}", paths.data.display());
    println!("permissions: application-level policy (not an operating-system sandbox)");
    Ok(())
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct FileConfig {
    execution: BTreeMap<String, serde_json::Value>,
    defaults: Option<DefaultsSection>,
    experimental: Option<ExperimentalSection>,
    permissions: Option<BTreeMap<String, toml::Value>>,
    compaction: Option<CompactionSection>,
    snapshots: Option<SnapshotsSection>,
    tools: Option<ToolsSection>,
    #[serde(alias = "toolAvailability")]
    tool_availability: BTreeMap<String, bool>,
    #[serde(alias = "lazyTools")]
    lazy_tools: Option<LazyToolsSection>,
    lsp: Option<haarness_language::LspConfig>,
    formatter: Option<haarness_language::FormatterConfig>,
    providers: BTreeMap<String, ProviderSection>,
    mcp: BTreeMap<String, McpConfigureRequest>,
    /// Opt-in discovery of Claude Code home/project files.
    compatibility: Option<CompatibilitySection>,
    /// Extra skill folders and remote skill indexes (`skills`).
    skills: Option<SkillsSection>,
}

/// `[compatibility]` / `{"compatibility": …}`: ecosystem file discovery that
/// is off unless explicitly enabled.
#[derive(Default, Clone, Deserialize)]
#[serde(default)]
struct CompatibilitySection {
    #[serde(alias = "claudeCode")]
    claude_code: Option<ClaudeCodeCompatibility>,
}

/// `compatibility.claude_code`: every category defaults to off, and
/// `enabled = false` switches all of them off regardless of the individual
/// toggles (the application master switch).
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
struct ClaudeCodeCompatibility {
    enabled: Option<bool>,
    /// `CLAUDE.md`, `.claude/CLAUDE.md`, `CONTEXT.md`, `*.local.md` and
    /// `~/.claude/CLAUDE.md` join the instruction set.
    instructions: Option<bool>,
    /// `.claude/agents/*.md` (project walk-up and `~/.claude/agents`).
    agents: Option<bool>,
    /// `.claude/skills` and `~/.claude/skills`.
    skills: Option<bool>,
    /// `.claude/commands` and `~/.claude/commands`.
    commands: Option<bool>,
}
impl ClaudeCodeCompatibility {
    fn category(&self, toggle: Option<bool>) -> bool {
        self.enabled != Some(false) && toggle == Some(true)
    }
    fn instructions(&self) -> bool {
        self.category(self.instructions)
    }
    fn agents(&self) -> bool {
        self.category(self.agents)
    }
    fn skills(&self) -> bool {
        self.category(self.skills)
    }
    fn commands(&self) -> bool {
        self.category(self.commands)
    }
    fn any(&self) -> bool {
        self.instructions() || self.agents() || self.skills() || self.commands()
    }
}

/// The effective Claude Code compatibility toggles of a merged configuration.
fn claude_compat(config: &FileConfig) -> ClaudeCodeCompatibility {
    config
        .compatibility
        .as_ref()
        .and_then(|section| section.claude_code.clone())
        .unwrap_or_default()
}

/// `skills`: `paths` are additional skill roots (`~/` and workspace-relative
/// paths allowed); `urls` name remote skill indexes whose `index.json` lists
/// skills to cache under the data directory.
#[derive(Default, Clone, Deserialize)]
#[serde(default)]
struct SkillsSection {
    paths: Vec<String>,
    urls: Vec<String>,
}

/// `[providers.<id>]`: a supported custom wire protocol, a preset override,
/// or `enabled = false` to suppress a preset/auto-detected provider.
#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
struct ProviderSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    integration: Option<router_connection::Integration>,
    /// Wire protocol. `kind` remains a deserialization alias for configs made
    /// before the protocol boundary was named explicitly.
    #[serde(alias = "kind")]
    protocol: String,
    name: Option<String>,
    base_url: Option<String>,
    api_key_env: Option<String>,
    api_key_envs: Vec<String>,
    /// Additional request headers. A value exactly matching `${NAME}` is read
    /// from that environment variable without storing the secret in TOML.
    headers: BTreeMap<String, String>,
    models: Vec<String>,
    reasoning: Option<bool>,
    images: Option<bool>,
    video: Option<bool>,
    tools: Option<bool>,
    context_window: Option<u64>,
    max_output_tokens: Option<u64>,
    /// Let managed Chat Completions gateways choose a budget-aware output allowance.
    server_output_limit: bool,
    /// Set false only for an endpoint that intentionally accepts unauthenticated calls.
    requires_auth: bool,
    enabled: bool,
}
impl Default for ProviderSection {
    fn default() -> Self {
        Self {
            protocol: "openai-chat-completions".into(),
            integration: None,
            name: None,
            base_url: None,
            api_key_env: None,
            api_key_envs: Vec::new(),
            headers: BTreeMap::new(),
            models: Vec::new(),
            reasoning: None,
            images: None,
            video: None,
            tools: None,
            context_window: None,
            max_output_tokens: None,
            server_output_limit: false,
            requires_auth: true,
            enabled: true,
        }
    }
}

/// Compile `[providers.*]` into protocol-specific instances plus the set of
/// suppressed provider ids.
fn provider_instances(
    config: &FileConfig,
    credentials: &HashMap<String, String>,
) -> (
    Vec<haarness_providers::ProviderInstance>,
    std::collections::HashSet<String>,
) {
    let mut instances = Vec::new();
    let mut disabled = std::collections::HashSet::new();
    for (id, section) in &config.providers {
        if !section.enabled {
            disabled.insert(id.clone());
            continue;
        }
        let Ok(protocol) = haarness_providers::CompatibleProtocol::parse(&section.protocol) else {
            eprintln!(
                "Provider {id}: unsupported protocol {}; skipped.",
                section.protocol
            );
            continue;
        };
        if section.video == Some(true)
            && !matches!(
                protocol,
                haarness_providers::CompatibleProtocol::GeminiGenerateContent
                    | haarness_providers::CompatibleProtocol::GoogleVertex
            )
        {
            eprintln!(
                "Provider {id}: video=true is supported only by gemini-generate-content; skipped."
            );
            continue;
        }
        let headers = match provider_headers(&section.headers) {
            Ok(headers) => headers,
            Err(error) => {
                eprintln!("Provider {id}: {error}; skipped.");
                continue;
            }
        };
        let preset = haarness_providers::compatible_preset(id);
        if preset.is_some()
            && protocol != haarness_providers::CompatibleProtocol::OpenAiChatCompletions
        {
            eprintln!("Provider {id}: preset protocol mismatch; skipped.");
            continue;
        }
        if preset.is_some_and(|preset| preset.protocol != protocol) {
            eprintln!("Provider {id}: preset protocol mismatch; skipped.");
            continue;
        }
        let Some(base_url) = section
            .base_url
            .clone()
            .or_else(|| preset.map(|preset| preset.base_url.to_owned()))
        else {
            eprintln!("Provider {id}: base_url is required; skipped.");
            continue;
        };
        if !valid_provider_url(&base_url) {
            eprintln!(
                "Provider {id}: base_url must use https, or http on an exact loopback host; skipped."
            );
            continue;
        }
        let api_key = section
            .api_key_env
            .iter()
            .chain(section.api_key_envs.iter())
            .find_map(|variable| std::env::var(variable).ok())
            .or_else(|| {
                preset.and_then(|preset| {
                    preset
                        .api_key_envs
                        .iter()
                        .find_map(|variable| std::env::var(variable).ok())
                })
            });
        let stored = credentials.contains_key(id)
            || preset.is_some_and(|preset| {
                credentials.contains_key(preset.id)
                    || preset
                        .aliases
                        .iter()
                        .any(|alias| credentials.contains_key(*alias))
            });
        let authenticated =
            api_key.is_some() || stored || !section.requires_auth || !headers.is_empty();
        let discover_models = section.models.is_empty()
            && authenticated
            && preset.is_none_or(|preset| preset.discover_models);
        let defaults = haarness_providers::CompatibleModelDefaults {
            reasoning: section.reasoning,
            images: section.images,
            video: section.video,
            tools: section.tools,
            context_window: section.context_window,
            max_output_tokens: section.max_output_tokens,
        };
        let instance = haarness_providers::NativeInstance {
            id: id.clone(),
            name: section.name.clone().unwrap_or_else(|| {
                preset.map_or_else(|| id.clone(), |preset| preset.name.to_owned())
            }),
            base_url,
            api_key,
            models: if section.models.is_empty() {
                preset.map_or_else(Vec::new, |preset| {
                    preset
                        .models
                        .iter()
                        .map(|model| (*model).to_owned())
                        .collect()
                })
            } else {
                section.models.clone()
            },
            discover_models,
            model_defaults: defaults,
            assume_authenticated: !section.requires_auth,
            headers,
        };
        instances.push(match protocol {
            haarness_providers::CompatibleProtocol::OpenAiChatCompletions => {
                haarness_providers::ProviderInstance::OpenAiChatCompletions(
                    haarness_providers::CompatibleInstance {
                        id: instance.id,
                        name: instance.name,
                        base_url: instance.base_url,
                        api_key: instance.api_key,
                        models: instance.models,
                        discover_models: instance.discover_models,
                        server_output_limit: section.server_output_limit,
                        model_defaults: instance.model_defaults,
                        assume_authenticated: instance.assume_authenticated,
                        headers: instance.headers,
                    },
                )
            }
            haarness_providers::CompatibleProtocol::AnthropicMessages => {
                haarness_providers::ProviderInstance::AnthropicMessages(instance)
            }
            haarness_providers::CompatibleProtocol::GeminiGenerateContent => {
                haarness_providers::ProviderInstance::GeminiGenerateContent(instance)
            }
            haarness_providers::CompatibleProtocol::AzureOpenAi => {
                haarness_providers::ProviderInstance::AzureOpenAi(instance)
            }
            haarness_providers::CompatibleProtocol::GoogleVertex => {
                haarness_providers::ProviderInstance::GoogleVertex(instance)
            }
            haarness_providers::CompatibleProtocol::GithubCopilot => {
                haarness_providers::ProviderInstance::GithubCopilot(instance)
            }
            haarness_providers::CompatibleProtocol::AmazonBedrock => {
                haarness_providers::ProviderInstance::AmazonBedrock(instance)
            }
        });
    }
    (instances, disabled)
}

fn validate_provider_definitions(config: &FileConfig) -> Result<()> {
    for (id, section) in &config.providers {
        if !section.enabled {
            continue;
        }
        let protocol = haarness_providers::CompatibleProtocol::parse(&section.protocol)
            .map_err(|error| anyhow::anyhow!("provider {id}: {error}"))?;
        if section.video == Some(true)
            && !matches!(
                protocol,
                haarness_providers::CompatibleProtocol::GeminiGenerateContent
                    | haarness_providers::CompatibleProtocol::GoogleVertex
            )
        {
            anyhow::bail!("provider {id}: video=true is supported only by gemini-generate-content");
        }
        provider_headers(&section.headers)
            .map_err(|error| anyhow::anyhow!("provider {id}: {error}"))?;
        let preset = haarness_providers::compatible_preset(id);
        if preset.is_some_and(|preset| preset.protocol != protocol) {
            anyhow::bail!("provider {id}: preset protocol mismatch");
        }
        let base_url = section
            .base_url
            .as_deref()
            .or_else(|| preset.map(|preset| preset.base_url))
            .with_context(|| format!("provider {id}: base_url is required"))?;
        if !valid_provider_url(base_url) {
            anyhow::bail!(
                "provider {id}: base_url must use https, or http on an exact loopback host"
            );
        }
    }
    Ok(())
}

fn provider_headers(
    configured: &BTreeMap<String, String>,
) -> std::result::Result<haarness_providers::ProviderHeaders, String> {
    let mut headers = BTreeMap::new();
    for (name, value) in configured {
        let value = if let Some(variable) = value
            .strip_prefix("${")
            .and_then(|value| value.strip_suffix('}'))
        {
            if variable.is_empty() || variable.contains(['{', '}']) {
                return Err(format!("invalid environment reference for header {name}"));
            }
            std::env::var(variable).map_err(|_| {
                format!("environment variable {variable} for header {name} is not set")
            })?
        } else {
            value.clone()
        };
        headers.insert(name.clone(), value);
    }
    haarness_providers::ProviderHeaders::parse(headers)
}

#[derive(Default, Clone, Deserialize)]
#[serde(default)]
struct DefaultsSection {
    provider: Option<String>,
    model: Option<String>,
    agent: Option<String>,
    small_model: Option<String>,
    reasoning_effort: Option<String>,
    ultracode: Option<bool>,
    output_style: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(default)]
struct CompactionSection {
    enabled: bool,
    threshold: f64,
    reserve_output: Option<u64>,
    tail_turns: usize,
    preserve_recent_tokens: u64,
}
impl Default for CompactionSection {
    fn default() -> Self {
        let defaults = haarness_core::CompactionSettings::default();
        Self {
            enabled: defaults.enabled,
            threshold: defaults.threshold,
            reserve_output: defaults.reserve_output,
            tail_turns: defaults.tail_turns,
            preserve_recent_tokens: defaults.preserve_recent_tokens,
        }
    }
}

#[derive(Default, Clone, Deserialize)]
#[serde(default)]
struct ToolsSection {
    max_rounds: Option<u32>,
}

#[derive(Clone, Deserialize)]
#[serde(default)]
struct LazyToolsSection {
    mode: haarness_protocol::LazyToolMode,
    #[serde(alias = "minSavedTokens")]
    min_saved_tokens: u64,
    pin: Vec<String>,
}

impl Default for LazyToolsSection {
    fn default() -> Self {
        let defaults = haarness_core::LazyToolSettings::default();
        Self {
            mode: defaults.mode,
            min_saved_tokens: defaults.min_saved_tokens,
            pin: defaults.pin,
        }
    }
}

#[derive(Default, Clone, Deserialize)]
#[serde(default)]
struct ExperimentalSection {
    #[serde(alias = "goalHarness")]
    goal_harness: Option<GoalHarnessSection>,
}

#[derive(Default, Clone, Deserialize)]
#[serde(default)]
struct GoalHarnessSection {
    #[serde(alias = "tokenBudget")]
    token_budget: Option<u64>,
    #[serde(alias = "maxTurns")]
    max_turns: Option<u32>,
}

#[derive(Clone, Deserialize)]
#[serde(default)]
struct SnapshotsSection {
    enabled: bool,
    /// Days a checkpoint stays restorable. Without a bound the blob store is
    /// append-only: every turn that touches a file keeps another copy forever.
    #[serde(alias = "retentionDays")]
    retention_days: u32,
    /// Checkpoints always kept per thread, however old, so `/undo` still has
    /// somewhere to go in a conversation nobody has touched in a month.
    #[serde(alias = "keepPerThread")]
    keep_per_thread: usize,
}
impl Default for SnapshotsSection {
    fn default() -> Self {
        Self {
            enabled: true,
            retention_days: 7,
            keep_per_thread: 20,
        }
    }
}

/// Reclaim expired checkpoints once at startup and then hourly. Pruning is
/// blocking SQLite work, so it runs on the blocking pool and never competes
/// with the turn path; a failure is logged and retried on the next pass.
// Automatically checkpoint repositories, without scanning an
// entire general folder such as Downloads. Plain folders can opt in explicitly.
fn automatic_snapshots_enabled(config: Option<&SnapshotsSection>, git: bool) -> bool {
    config.map_or(git, |config| config.enabled)
}

fn spawn_snapshot_retention(
    snapshots: Arc<haarness_store::SqliteSnapshotStore>,
    config: &SnapshotsSection,
) {
    if config.retention_days == 0 {
        return;
    }
    let retention = chrono::Duration::days(i64::from(config.retention_days));
    let keep = config.keep_per_thread;
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60 * 60));
        loop {
            ticker.tick().await;
            let snapshots = Arc::clone(&snapshots);
            let report =
                tokio::task::spawn_blocking(move || snapshots.prune(retention, keep)).await;
            match report {
                Ok(Ok(report)) if report.snapshots > 0 => tracing::info!(
                    snapshots = report.snapshots,
                    blobs = report.blobs,
                    "pruned expired workspace checkpoints"
                ),
                Ok(Ok(_)) => {}
                Ok(Err(error)) => tracing::warn!(%error, "snapshot retention pass failed"),
                Err(error) => tracing::warn!(%error, "snapshot retention task panicked"),
            }
        }
    });
}

fn load_config(paths: &Paths, workspace: &Path, trust: bool) -> Result<FileConfig> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let layers =
        jsonconfig::load_layers(paths.config_dir(), home.as_deref(), workspace, trust, &[]);
    if !layers.errors.is_empty() {
        anyhow::bail!(layers.errors.join("; "));
    }
    let mut config = layers.native;
    config.mcp.extend(layers.mcp);
    Ok(config)
}

fn merge_file_config(config: &mut FileConfig, project_config: FileConfig) {
    config.execution.extend(project_config.execution);
    config.mcp.extend(project_config.mcp);
    if let Some(defaults) = project_config.defaults {
        let merged = config.defaults.get_or_insert_with(DefaultsSection::default);
        merge_defaults(merged, defaults);
    }
    if let Some(project_permissions) = project_config.permissions {
        // Project rules append after global rules: last match wins.
        config
            .permissions
            .get_or_insert_with(BTreeMap::new)
            .extend(project_permissions);
    }
    if project_config.compaction.is_some() {
        config.compaction = project_config.compaction;
    }
    if project_config.tools.is_some() {
        config.tools = project_config.tools;
    }
    config
        .tool_availability
        .extend(project_config.tool_availability);
    if project_config.lazy_tools.is_some() {
        config.lazy_tools = project_config.lazy_tools;
    }
    if project_config.lsp.is_some() {
        config.lsp = project_config.lsp;
    }
    if project_config.formatter.is_some() {
        config.formatter = project_config.formatter;
    }
    if let Some(project) = project_config.experimental {
        merge_experimental(
            config
                .experimental
                .get_or_insert_with(ExperimentalSection::default),
            project,
        );
    }
    if project_config.snapshots.is_some() {
        config.snapshots = project_config.snapshots;
    }
    config.providers.extend(project_config.providers);
    if let Some(project) = project_config.compatibility {
        let base = config
            .compatibility
            .get_or_insert_with(CompatibilitySection::default);
        if let Some(project) = project.claude_code {
            let claude = base
                .claude_code
                .get_or_insert_with(ClaudeCodeCompatibility::default);
            for (slot, value) in [
                (&mut claude.enabled, project.enabled),
                (&mut claude.instructions, project.instructions),
                (&mut claude.agents, project.agents),
                (&mut claude.skills, project.skills),
                (&mut claude.commands, project.commands),
            ] {
                if value.is_some() {
                    *slot = value;
                }
            }
        }
    }
    if let Some(project) = project_config.skills {
        let base = config.skills.get_or_insert_with(SkillsSection::default);
        for path in project.paths {
            if !base.paths.contains(&path) {
                base.paths.push(path);
            }
        }
        for url in project.urls {
            if !base.urls.contains(&url) {
                base.urls.push(url);
            }
        }
    }
}

fn merge_defaults(base: &mut DefaultsSection, project: DefaultsSection) {
    if project.provider.is_some() {
        base.provider = project.provider;
    }
    if project.model.is_some() {
        base.model = project.model;
    }
    if project.agent.is_some() {
        base.agent = project.agent;
    }
    if project.small_model.is_some() {
        base.small_model = project.small_model;
    }
    if project.reasoning_effort.is_some() {
        base.reasoning_effort = project.reasoning_effort;
    }
    if project.ultracode.is_some() {
        base.ultracode = project.ultracode;
    }
    if project.output_style.is_some() {
        base.output_style = project.output_style;
    }
}

fn merge_experimental(base: &mut ExperimentalSection, project: ExperimentalSection) {
    let Some(project) = project.goal_harness else {
        return;
    };
    let base = base
        .goal_harness
        .get_or_insert_with(GoalHarnessSection::default);
    if project.token_budget.is_some() {
        base.token_budget = project.token_budget;
    }
    if project.max_turns.is_some() {
        base.max_turns = project.max_turns;
    }
}

fn apply_json_goal_config(config: &mut FileConfig, layers: &jsonconfig::JsonLayers) {
    if layers.goal_token_budget.is_none() && layers.goal_max_turns.is_none() {
        return;
    }
    let goal = config
        .experimental
        .get_or_insert_with(ExperimentalSection::default)
        .goal_harness
        .get_or_insert_with(GoalHarnessSection::default);
    if layers.goal_token_budget.is_some() {
        goal.token_budget = layers.goal_token_budget;
    }
    if layers.goal_max_turns.is_some() {
        goal.max_turns = layers.goal_max_turns;
    }
}

/// Compile `[permissions]` TOML into ordered engine rules. Tool-level string
/// entries compile first (`edit = "ask"` → `edit:*`), then per-tool pattern
/// tables (`[permissions.bash] "git push*" = "ask"` → `bash:git push*`), so
/// specific patterns override tool-level decisions under last-match-wins.
fn compile_permissions(
    map: &BTreeMap<String, toml::Value>,
) -> Result<Vec<haarness_core::PermissionRule>> {
    use haarness_core::{PermissionDecision, PermissionRule};
    fn decision(value: &str) -> Result<PermissionDecision> {
        match value {
            "allow" => Ok(PermissionDecision::Allow),
            "ask" => Ok(PermissionDecision::Ask),
            "deny" => Ok(PermissionDecision::Deny),
            other => anyhow::bail!("permission decision must be allow|ask|deny, got {other}"),
        }
    }
    let mut rules = Vec::new();
    for (tool, value) in map {
        if let toml::Value::String(text) = value {
            let pattern = if tool == "*" {
                "*".to_owned()
            } else {
                format!("{tool}:*")
            };
            rules.push(PermissionRule {
                pattern,
                decision: decision(text)?,
            });
        }
    }
    for (tool, value) in map {
        if let toml::Value::Table(patterns) = value {
            for (pattern, entry) in patterns {
                let text = entry.as_str().ok_or_else(|| {
                    anyhow::anyhow!("[permissions.{tool}] entries must be strings")
                })?;
                rules.push(PermissionRule {
                    pattern: format!("{tool}:{pattern}"),
                    decision: decision(text)?,
                });
            }
        }
    }
    Ok(rules)
}

fn runtime_config(
    config: &FileConfig,
    json_rules: Vec<haarness_core::PermissionRule>,
    persisted_rules: Vec<haarness_core::PermissionRule>,
    auto_approve: bool,
) -> Result<haarness_core::RuntimeConfig> {
    let mut base_rules = Vec::new();
    if let Some(permissions) = &config.permissions {
        base_rules.extend(compile_permissions(permissions)?);
    }
    // JSON/Claude-compat rules follow TOML rules (later files win)…
    base_rules.extend(json_rules);
    // …and interactive "always for this workspace" approvals apply after all
    // config rules, before session approvals.
    base_rules.extend(persisted_rules);
    let defaults = config.defaults.clone().unwrap_or_default();
    let compaction = config.compaction.clone().unwrap_or_default();
    let goal_harness = config
        .experimental
        .as_ref()
        .and_then(|experimental| experimental.goal_harness.as_ref());
    if goal_harness.is_some_and(|goal| goal.token_budget == Some(0)) {
        anyhow::bail!("experimental.goal_harness.token_budget must be greater than zero");
    }
    if goal_harness.is_some_and(|goal| goal.max_turns == Some(0)) {
        anyhow::bail!("experimental.goal_harness.max_turns must be greater than zero");
    }
    if config
        .lazy_tools
        .as_ref()
        .is_some_and(|settings| settings.min_saved_tokens == 0)
    {
        anyhow::bail!("lazy_tools.min_saved_tokens must be greater than zero");
    }
    if !(0.0 < compaction.threshold && compaction.threshold <= 1.0) {
        anyhow::bail!("compaction.threshold must be greater than 0 and at most 1");
    }
    if compaction.tail_turns == 0 {
        anyhow::bail!("compaction.tail_turns must be greater than zero");
    }
    if compaction.preserve_recent_tokens == 0 {
        anyhow::bail!("compaction.preserve_recent_tokens must be greater than zero");
    }
    Ok(haarness_core::RuntimeConfig {
        execution: {
            let settings: haarness_core::ExecutionSettings =
                serde_json::from_value(serde_json::to_value(&config.execution)?)?;
            settings.validate()?;
            settings
        },
        read_roots: Vec::new(),
        base_rules,
        default_agent: defaults.agent,
        default_provider: defaults.provider,
        default_model: defaults.model,
        small_model: defaults.small_model,
        default_reasoning_effort: defaults.reasoning_effort,
        default_ultracode: defaults.ultracode.unwrap_or(false),
        output_style: None,
        max_rounds: config.tools.clone().unwrap_or_default().max_rounds,
        tool_availability: config
            .tool_availability
            .iter()
            .map(|(name, enabled)| (name.clone(), *enabled))
            .collect(),
        lazy_tools: config.lazy_tools.as_ref().map_or_else(
            haarness_core::LazyToolSettings::default,
            |settings| haarness_core::LazyToolSettings {
                mode: settings.mode,
                min_saved_tokens: settings.min_saved_tokens,
                pin: settings.pin.clone(),
            },
        ),
        goal_token_budget: goal_harness.and_then(|goal| goal.token_budget),
        goal_max_turns: goal_harness.and_then(|goal| goal.max_turns),
        compaction: haarness_core::CompactionSettings {
            enabled: compaction.enabled,
            threshold: compaction.threshold,
            reserve_output: compaction.reserve_output,
            tail_turns: compaction.tail_turns,
            preserve_recent_tokens: compaction.preserve_recent_tokens,
        },
        auto_approve,
        commands: Vec::new(),
        custom_agents: Vec::new(),
        disabled_agents: Vec::new(),
    })
}

fn plugin_command(
    paths: &Paths,
    workspace: &Path,
    project_config: bool,
    command: &PluginCommand,
) -> Result<()> {
    match command {
        PluginCommand::Marketplace {
            source,
            compatibility_source,
        } => {
            let source = match (source.as_str(), compatibility_source.as_deref()) {
                ("add", Some(source)) => source,
                ("add", None) => anyhow::bail!("plugin marketplace add requires a source"),
                (_, None) => source,
                (_, Some(_)) => anyhow::bail!(
                    "use 'plugin marketplace <source>' or 'plugin marketplace add <source>'"
                ),
            };
            let name = plugins::marketplace_add(&paths.data, source)?;
            println!(
                "Marketplace '{name}' registered. Install with: haarness plugin install <plugin>@{name}"
            );
        }
        PluginCommand::Install { spec } => {
            let plugin = plugins::install(&paths.data, spec)?;
            println!(
                "Installed {spec} → {} (enabled). A running Haarness server will reload it when active work is idle.",
                plugin.path.display()
            );
        }
        PluginCommand::Update { name } => {
            let plugin = plugins::update(&paths.data, name)?;
            println!(
                "Updated {name} → {}. A running Haarness server will reload it when active work is idle.",
                plugin.path.display()
            );
        }
        PluginCommand::List => {
            let marketplaces = plugins::marketplaces(&paths.data);
            if marketplaces.is_empty() {
                println!("No marketplaces registered (haarness plugin marketplace <source>).");
            } else {
                println!("Marketplaces:");
                for (name, path) in marketplaces {
                    println!("  {name}  {}", path.display());
                }
            }
            let installed =
                plugins::plugin_infos(&paths.data, paths.config_dir(), workspace, project_config);
            if installed.is_empty() {
                println!("No plugins installed.");
            } else {
                println!("Plugins:");
                for plugin in installed {
                    println!(
                        "  {}  {}  {}  {}",
                        if plugin.enabled { "●" } else { "○" },
                        plugin.name,
                        plugin.source,
                        plugin.description
                    );
                }
            }
            let declared =
                config_specs::declarations(paths.config_dir(), workspace, project_config);
            let pending = config_specs::pending_plugins(&paths.data, &declared.plugins);
            if !pending.is_empty() {
                println!("Declared in configuration but not installed:");
                for plugin in pending {
                    println!(
                        "  ○  {}  ({})  run: haarness plugin install {}",
                        plugin.spec,
                        plugin.file.display(),
                        plugin.spec
                    );
                }
            }
        }
        PluginCommand::Enable { name } => {
            plugins::set_any_enabled(&paths.data, paths.config_dir(), workspace, name, true)?;
            println!(
                "Enabled {name}. A running Haarness server will reload it when active work is idle."
            );
        }
        PluginCommand::Disable { name } => {
            plugins::set_any_enabled(&paths.data, paths.config_dir(), workspace, name, false)?;
            println!("Disabled {name}.");
        }
        PluginCommand::Remove { name } => {
            plugins::remove_any(&paths.data, paths.config_dir(), workspace, name)?;
            println!("Removed {name}.");
        }
    }
    Ok(())
}

/// Keeps asynchronous file logs alive and flushes an optional OTLP provider.
struct TelemetryGuard {
    _file_writer: Option<tracing_appender::non_blocking::WorkerGuard>,
    tracer_provider: Option<SdkTracerProvider>,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(provider) = &self.tracer_provider
            && let Err(error) = provider.shutdown()
        {
            eprintln!("failed to flush OpenTelemetry traces: {error}");
        }
    }
}

/// Rotating file logging (daily, under `<data>/logs/`), optional stderr
/// streaming, and opt-in OTLP/HTTP trace export. The returned guard must live
/// for the whole process so both asynchronous pipelines flush on shutdown.
fn init_logging(
    paths: &Paths,
    level: Option<&str>,
    stderr_logs: bool,
    file_logs: bool,
) -> Result<TelemetryGuard> {
    use tracing_subscriber::Layer as _;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    let log_filter = match level {
        Some(level) => EnvFilter::new(haarness_filter(level)),
        None => EnvFilter::try_from_env("HAARNESS_LOG")
            .or_else(|_| EnvFilter::try_from_default_env())
            .unwrap_or_else(|_| EnvFilter::new(haarness_filter("info"))),
    };
    let mut layers: Vec<
        Box<dyn tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync>,
    > = Vec::new();
    let file_writer_guard = if file_logs {
        let logs = paths.data.join("logs");
        fs::create_dir_all(&logs)?;
        let (file_writer, guard) =
            tracing_appender::non_blocking(tracing_appender::rolling::daily(&logs, "haarness.log"));
        layers.push(Box::new(
            tracing_subscriber::fmt::layer()
                .with_writer(file_writer)
                .with_ansi(false)
                .with_filter(log_filter.clone()),
        ));
        Some(guard)
    } else {
        None
    };
    if stderr_logs {
        layers.push(Box::new(
            tracing_subscriber::fmt::layer()
                .with_writer(io::stderr)
                .with_filter(log_filter),
        ));
    }
    let tracer_provider = init_tracer_provider()?;
    if let Some(provider) = &tracer_provider {
        let telemetry_filter = EnvFilter::try_from_env("HAARNESS_OTEL_FILTER")
            .unwrap_or_else(|_| EnvFilter::new(haarness_filter("info")));
        let tracer = provider.tracer("haarness");
        layers.push(Box::new(
            tracing_opentelemetry::layer()
                .with_tracer(tracer)
                .with_location(true)
                .with_threads(true)
                .with_filter(telemetry_filter),
        ));
    }
    tracing_subscriber::registry()
        .with(layers)
        .try_init()
        .context("initialize tracing subscriber")?;
    Ok(TelemetryGuard {
        _file_writer: file_writer_guard,
        tracer_provider,
    })
}

fn init_tracer_provider() -> Result<Option<SdkTracerProvider>> {
    if std::env::var("OTEL_SDK_DISABLED").is_ok_and(|value| value.eq_ignore_ascii_case("true"))
        || ![
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            "OTEL_EXPORTER_OTLP_ENDPOINT",
        ]
        .into_iter()
        .any(|name| std::env::var(name).is_ok_and(|value| !value.trim().is_empty()))
    {
        return Ok(None);
    }

    let protocol = std::env::var("OTEL_EXPORTER_OTLP_TRACES_PROTOCOL")
        .ok()
        .or_else(|| std::env::var("OTEL_EXPORTER_OTLP_PROTOCOL").ok());
    if let Some(protocol) = protocol
        && !protocol.eq_ignore_ascii_case("http/protobuf")
    {
        anyhow::bail!(
            "unsupported OTLP trace protocol {protocol:?}; Haarness supports http/protobuf"
        );
    }

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .build()
        .context("configure OTLP trace exporter")?;
    let service_name = std::env::var("OTEL_SERVICE_NAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "haarness".to_owned());
    let resource = Resource::builder()
        .with_service_name(service_name)
        .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
        .build();
    let provider = SdkTracerProvider::builder()
        .with_sampler(otel_sampler_from_env()?)
        .with_resource(resource)
        .with_batch_exporter(exporter)
        .build();
    global::set_text_map_propagator(TraceContextPropagator::new());
    global::set_tracer_provider(provider.clone());
    Ok(Some(provider))
}

fn otel_sampler_from_env() -> Result<Sampler> {
    let configured = std::env::var("OTEL_TRACES_SAMPLER")
        .unwrap_or_else(|_| "parentbased_always_on".to_owned())
        .to_ascii_lowercase();
    let ratio = || -> Result<f64> {
        let value = std::env::var("OTEL_TRACES_SAMPLER_ARG")
            .unwrap_or_else(|_| "1.0".to_owned())
            .parse::<f64>()
            .context("OTEL_TRACES_SAMPLER_ARG must be a number between 0 and 1")?;
        if !(0.0..=1.0).contains(&value) {
            anyhow::bail!("OTEL_TRACES_SAMPLER_ARG must be between 0 and 1");
        }
        Ok(value)
    };
    match configured.as_str() {
        "always_on" => Ok(Sampler::AlwaysOn),
        "always_off" => Ok(Sampler::AlwaysOff),
        "traceidratio" => Ok(Sampler::TraceIdRatioBased(ratio()?)),
        "parentbased_always_on" => Ok(Sampler::ParentBased(Box::new(Sampler::AlwaysOn))),
        "parentbased_always_off" => Ok(Sampler::ParentBased(Box::new(Sampler::AlwaysOff))),
        "parentbased_traceidratio" => Ok(Sampler::ParentBased(Box::new(
            Sampler::TraceIdRatioBased(ratio()?),
        ))),
        _ => anyhow::bail!("unsupported OTEL_TRACES_SAMPLER value {configured:?}"),
    }
}

/// `warn` globally, the chosen level for every haarness crate.
fn haarness_filter(level: &str) -> String {
    use std::fmt::Write as _;
    let mut filter = String::from("warn");
    for name in [
        "haarness",
        "haarness_cli",
        "haarness_core",
        "haarness_language",
        "haarness_server",
        "haarness_providers",
        "haarness_store",
        "haarness_tui",
        "haarness_mcp",
        "haarness_opentui",
        "haarness_pty",
        "haarness_scheduler",
        "haarness_workflow_catalog",
        "haarness_workflows",
        "haarness_workspaces",
    ] {
        let _ = write!(filter, ",{name}={level}");
    }
    filter
}

/// `haarness logs`: show the log directory and files, or tail the newest.
fn logs_command(paths: &Paths, tail: Option<usize>) -> Result<()> {
    let logs = paths.data.join("logs");
    let mut files: Vec<_> = fs::read_dir(&logs)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_file())
        .collect();
    files.sort_by_key(std::fs::DirEntry::file_name);
    if let Some(count) = tail {
        let Some(latest) = files.last() else {
            println!("No log files in {}", logs.display());
            return Ok(());
        };
        let content = fs::read_to_string(latest.path())?;
        let lines: Vec<&str> = content.lines().collect();
        let start = lines.len().saturating_sub(count);
        for line in &lines[start..] {
            println!("{line}");
        }
        return Ok(());
    }
    println!("{}", logs.display());
    if files.is_empty() {
        println!("  (no log files yet)");
    }
    for entry in &files {
        let size = entry.metadata().map_or(0, |meta| meta.len());
        println!(
            "  {}  {} KiB",
            entry.file_name().to_string_lossy(),
            size / 1024
        );
    }
    Ok(())
}

#[derive(Clone)]
struct Paths {
    root: Option<PathBuf>,
    data: PathBuf,
    cache: PathBuf,
    database: PathBuf,
    auth: PathBuf,
    runtime: PathBuf,
    server_lock: PathBuf,
    skills: PathBuf,
    config: PathBuf,
    workspace_id: Option<String>,
}
impl Paths {
    fn new(root: Option<&Path>) -> Result<Self> {
        if let Some(root) = root {
            let data = root.join("data");
            let config = root.join("config");
            let cache = root.join("cache");
            fs::create_dir_all(&data)?;
            fs::create_dir_all(&config)?;
            fs::create_dir_all(&cache)?;
            if !data.join(".layout-v1").exists() {
                layout::copy_missing(&config.join("auth.json"), &data.join("auth.json"))?;
                layout::copy_missing(&data.join("skills"), &config.join("skills"))?;
                layout::copy_missing(&data.join("plugins/local"), &config.join("plugins/local"))?;
                fs::write(
                    data.join(".layout-v1"),
                    "Migrated resource and credential locations.\n",
                )?;
            }
            return Ok(Self {
                root: Some(root.to_path_buf()),
                cache: cache.clone(),
                database: data.join("haarness.db"),
                auth: data.join("auth.json"),
                runtime: cache.join("runtime.json"),
                server_lock: cache.join("server.lock"),
                skills: config.join("skills"),
                config: config.join("config.toml"),
                workspace_id: None,
                data,
            });
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()))
            .context("home directory unavailable")?;
        let root = std::env::var_os("HAARNESS_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let layout = layout::Layout::resolve(&home, root.as_deref(), |key| {
            std::env::var_os(key).map(PathBuf::from)
        })?;
        let legacy = root
            .is_none()
            .then(|| directories::ProjectDirs::from("dev", "haarness", "haarness"))
            .flatten();
        layout.initialize(legacy.as_ref())?;
        Ok(Self {
            root: None,
            cache: layout.cache.clone(),
            database: layout.data.join("haarness.db"),
            auth: layout.data.join("auth.json"),
            runtime: layout.cache.join("runtime.json"),
            server_lock: layout.cache.join("server.lock"),
            skills: layout.config.join("skills"),
            config: layout.config.join("config.toml"),
            workspace_id: None,
            data: layout.data,
        })
    }

    fn config_dir(&self) -> &Path {
        self.config.parent().expect("config file has parent")
    }

    fn workspace_manager(&self) -> Result<WorkspaceManager> {
        WorkspaceManager::new(
            self.data.join("workspaces.json"),
            self.data.join("worktrees"),
            self.cache.join("workspaces"),
        )
        .map_err(anyhow::Error::new)
    }

    fn scoped(&self, workspace_id: &str, migrate_legacy: bool) -> Result<Self> {
        let data = self.data.join("workspaces").join(workspace_id);
        let cache = self.cache.join("workspaces").join(workspace_id);
        fs::create_dir_all(&data)?;
        fs::create_dir_all(&cache)?;
        let database = data.join("haarness.db");
        if migrate_legacy && !database.exists() && self.data.join("haarness.db").exists() {
            migrate_legacy_workspace_database(self, &database)?;
        }
        Ok(Self {
            database,
            runtime: cache.join("runtime.json"),
            server_lock: cache.join("server.lock"),
            workspace_id: Some(workspace_id.to_owned()),
            ..self.clone()
        })
    }
}

/// Copy the pre-workspace database into the first registered workspace. The
/// old global server lease must be available, preventing a live legacy daemon
/// from racing the migration. `VACUUM INTO` folds any WAL contents into a
/// self-contained destination before the legacy files are removed, so a
/// failed copy leaves the source authoritative and retryable.
fn migrate_legacy_workspace_database(paths: &Paths, destination: &Path) -> Result<()> {
    if destination.exists() {
        anyhow::bail!(
            "refusing to overwrite existing workspace database {}",
            destination.display()
        );
    }
    let legacy_lock_path = paths.cache.join("server.lock");
    let legacy_lock = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&legacy_lock_path)?;
    legacy_lock.try_lock_exclusive().with_context(|| {
        format!(
            "cannot migrate the legacy session database while its server is active ({})",
            legacy_lock_path.display()
        )
    })?;
    let legacy = paths.data.join("haarness.db");
    let migration = (|| -> Result<()> {
        let connection = rusqlite::Connection::open(&legacy)
            .with_context(|| format!("open legacy database {}", legacy.display()))?;
        connection.busy_timeout(Duration::from_secs(5))?;
        // A stopped WAL database can still contain committed pages. Checkpoint
        // first, then VACUUM INTO a new ordinary database rather than moving
        // the main file ahead of its sidecars.
        connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        connection.execute("VACUUM INTO ?1", [destination.display().to_string()])?;
        drop(connection);
        let check = rusqlite::Connection::open(destination)?.query_row(
            "PRAGMA integrity_check",
            [],
            |row| row.get::<_, String>(0),
        )?;
        if check != "ok" {
            anyhow::bail!("migrated session database failed integrity_check: {check}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(destination, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    })();
    if let Err(error) = migration {
        let _ = fs::remove_file(destination);
        return Err(error).with_context(|| {
            format!(
                "migrate legacy session database {} to {}",
                legacy.display(),
                destination.display()
            )
        });
    }
    fs::remove_file(&legacy)?;
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", legacy.display()));
        if let Err(error) = fs::remove_file(&sidecar)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(path = %sidecar.display(), %error, "could not remove migrated SQLite sidecar");
        }
    }
    Ok(())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct RuntimeDiscovery {
    url: String,
    token: String,
    pid: u32,
    /// Added after the first discovery format shipped. The live `/v1/info`
    /// response remains authoritative for legacy files.
    #[serde(default)]
    workspace: Option<String>,
    /// Identity of the executable that owns this local daemon. Package
    /// versions do not change between development rebuilds, so version/schema
    /// checks alone can otherwise attach a new client to stale server code.
    #[serde(default)]
    executable_fingerprint: Option<String>,
    #[serde(default)]
    access_fingerprint: Option<String>,
}
#[derive(Clone, Debug)]
struct LiveWorkspaceServer {
    discovery: RuntimeDiscovery,
    info: ServerInfo,
}
fn write_runtime(paths: &Paths, value: &RuntimeDiscovery) -> Result<()> {
    write_private_json(&paths.runtime, value)
}
fn read_runtime(paths: &Paths) -> Result<RuntimeDiscovery> {
    Ok(serde_json::from_slice(&fs::read(&paths.runtime)?)?)
}

fn same_server(left: &str, right: &str) -> bool {
    left.trim_end_matches('/') == right.trim_end_matches('/')
}

/// Resolve explicit attach credentials without assuming the server workspace
/// exists on this client. A matching private local rendezvous record is only a
/// credential convenience; authenticated `/v1/info` is authoritative.
async fn attached_endpoint(
    paths: &Paths,
    server: &str,
    token: Option<String>,
    authentication: PasswordArgs,
) -> Result<(String, haarness_tui::api::ServerAuth)> {
    let token = token
        .filter(|token| !token.trim().is_empty())
        .or_else(|| {
            read_runtime(paths)
                .ok()
                .filter(|runtime| same_server(&runtime.url, server))
                .map(|runtime| runtime.token)
                .filter(|token| !token.trim().is_empty())
        })
        .unwrap_or_default();
    let auth = if let Some(password) = authentication.password.filter(|value| !value.is_empty()) {
        haarness_tui::api::ServerAuth::basic(
            authentication.username.as_deref().unwrap_or("haarness"),
            &password,
        )?
    } else {
        haarness_tui::api::ServerAuth::bearer(&token)
    };
    let client = haarness_tui::api::ApiClient::with_auth(server, auth.clone())?;
    let info = tokio::time::timeout(Duration::from_secs(4), client.info())
        .await
        .with_context(|| format!("timed out connecting to Haarness server {server}"))?
        .with_context(|| format!("cannot attach to Haarness server {server}"))?;
    if let Some(reason) = server_compatibility_error(&info) {
        anyhow::bail!("Haarness server at {server} is incompatible with this client ({reason})");
    }
    Ok((server.trim_end_matches('/').to_owned(), auth))
}

/// Re-read the private local rendezvous file while a TUI is attached. A
/// replacement daemon receives a new ephemeral port and token, so retrying the
/// original URL forever cannot recover. Explicit `--connect` clients never use
/// this resolver and remain pinned to their user-selected endpoint.
fn runtime_endpoint_resolver(
    paths: &Paths,
    workspace: &Path,
) -> haarness_tui::api::EndpointResolver {
    let runtime = paths.runtime.clone();
    let expected_workspace = comparable_workspace(workspace);
    Arc::new(move || {
        let discovery =
            serde_json::from_slice::<RuntimeDiscovery>(&fs::read(&runtime).ok()?).ok()?;
        if discovery
            .workspace
            .as_deref()
            .is_some_and(|actual| comparable_workspace(Path::new(actual)) != expected_workspace)
        {
            return None;
        }
        Some(haarness_tui::api::EndpointCredentials::new(
            discovery.url,
            discovery.token,
        ))
    })
}

fn current_executable_fingerprint() -> Result<String> {
    let path = std::env::current_exe().context("cannot locate the Haarness executable")?;
    let path = path.canonicalize().unwrap_or(path);
    let metadata = fs::metadata(&path)
        .with_context(|| format!("cannot inspect Haarness executable {}", path.display()))?;
    let modified = metadata
        .modified()
        .context("Haarness executable modification time is unavailable")?
        .duration_since(UNIX_EPOCH)
        .context("Haarness executable modification time predates the Unix epoch")?
        .as_nanos();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Ok(format!(
            "{}:{}:{}:{}:{}",
            path.display(),
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            modified
        ))
    }
    #[cfg(not(unix))]
    {
        Ok(format!(
            "{}:{}:{}",
            path.display(),
            metadata.len(),
            modified
        ))
    }
}

/// Inspect the one live server associated with this data directory. Runtime
/// discovery is only a rendezvous hint: health and workspace identity are
/// always verified against the authenticated server before it is reused.
async fn inspect_workspace_server(
    paths: &Paths,
    expected_workspace: &Path,
) -> Result<Option<LiveWorkspaceServer>> {
    let discovery = match read_runtime(paths) {
        Ok(discovery) => discovery,
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::NotFound) =>
        {
            return Ok(None);
        }
        Err(error) => {
            tracing::warn!(%error, path = %paths.runtime.display(), "ignoring stale runtime discovery");
            return Ok(None);
        }
    };
    let client = match haarness_tui::api::ApiClient::new(&discovery.url, &discovery.token) {
        Ok(client) => client,
        Err(error) => {
            tracing::debug!(%error, "invalid runtime discovery");
            return Ok(None);
        }
    };
    let info = match tokio::time::timeout(Duration::from_millis(750), client.info()).await {
        Ok(Ok(info)) => info,
        Ok(Err(error)) => {
            tracing::debug!(%error, pid = discovery.pid, "runtime discovery is not reachable");
            return Ok(None);
        }
        Err(_) => {
            tracing::debug!(
                pid = discovery.pid,
                "runtime discovery health check timed out"
            );
            return Ok(None);
        }
    };

    if let Some(actual) = info.workspace.as_deref() {
        let actual = comparable_workspace(Path::new(actual));
        let expected = comparable_workspace(expected_workspace);
        if actual != expected {
            anyhow::bail!(
                "Haarness server {} is already running for {}; stop it with `haarness --workspace '{}' serve --stop` before using {} with the same data directory",
                discovery.pid,
                actual.display(),
                actual.display(),
                expected.display(),
            );
        }
    }
    Ok(Some(LiveWorkspaceServer { discovery, info }))
}

fn server_compatibility_error(info: &ServerInfo) -> Option<String> {
    if info.name != "haarness" {
        return Some(format!("unexpected server identity '{}'", info.name));
    }
    if info.schema_version != SCHEMA_VERSION {
        return Some(format!(
            "API schema {} is not supported by this client (expected {SCHEMA_VERSION})",
            info.schema_version
        ));
    }
    if !info
        .capabilities
        .iter()
        .any(|capability| capability == WORKSPACE_HISTORY_CAPABILITY)
    {
        return Some(format!(
            "missing required capability {WORKSPACE_HISTORY_CAPABILITY}"
        ));
    }
    if !info
        .capabilities
        .iter()
        .any(|capability| capability == PTY_CAPABILITY)
    {
        return Some(format!("missing required capability {PTY_CAPABILITY}"));
    }
    None
}

fn local_server_compatibility_error(live: &LiveWorkspaceServer) -> Option<String> {
    server_compatibility_error(&live.info).or_else(|| {
        let current = current_executable_fingerprint().ok()?;
        (live.discovery.executable_fingerprint.as_deref() != Some(current.as_str()))
            .then(|| "server executable differs from this client build".to_owned())
    })
}

/// Return only a server compatible with every feature exposed by this client.
/// Explicit `--connect` callers get an actionable error instead of attaching
/// successfully and discovering missing routes one slash command at a time.
async fn discover_workspace_server(
    paths: &Paths,
    expected_workspace: &Path,
) -> Result<Option<RuntimeDiscovery>> {
    let Some(live) = inspect_workspace_server(paths, expected_workspace).await? else {
        return Ok(None);
    };
    if let Some(reason) = local_server_compatibility_error(&live) {
        anyhow::bail!(
            "Haarness server {} is incompatible with this client ({reason}); restart it with `haarness --workspace '{}' serve --stop`",
            live.discovery.pid,
            expected_workspace.display()
        );
    }
    Ok(Some(live.discovery))
}

async fn replace_incompatible_workspace_server(
    paths: &Paths,
    expected_workspace: &Path,
    live: LiveWorkspaceServer,
    reason: &str,
) -> Result<()> {
    let client = haarness_tui::api::ApiClient::new(&live.discovery.url, &live.discovery.token)?;
    let active = tokio::time::timeout(Duration::from_millis(750), client.active_turns())
        .await
        .context("timed out checking the stale Haarness server for active turns")?
        .with_context(|| {
            format!(
                "cannot safely replace Haarness server {} ({reason}) because its activity endpoint is unavailable; stop it explicitly with `haarness --workspace '{}' serve --stop`",
                live.discovery.pid,
                expected_workspace.display()
            )
        })?;
    if !active.is_empty() {
        anyhow::bail!(
            "Haarness server {} needs an upgrade ({reason}) but still owns {} active turn(s); wait for them to finish or stop it explicitly with `haarness --workspace '{}' serve --stop`",
            live.discovery.pid,
            active.len(),
            expected_workspace.display()
        );
    }

    tracing::info!(
        pid = live.discovery.pid,
        %reason,
        "restarting incompatible Haarness workspace server"
    );
    client.shutdown_server().await.with_context(|| {
        format!(
            "could not stop incompatible Haarness server {}",
            live.discovery.pid
        )
    })?;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let still_recorded =
            read_runtime(paths).is_ok_and(|current| current.pid == live.discovery.pid);
        let still_reachable = tokio::time::timeout(Duration::from_millis(250), client.info())
            .await
            .is_ok_and(|result| result.is_ok());
        if !still_recorded && !still_reachable {
            // The process drops its workspace lease immediately after removing
            // discovery; give that final destructor a short scheduling turn.
            tokio::time::sleep(Duration::from_millis(50)).await;
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!(
                "incompatible Haarness server {} did not stop within 5 seconds",
                live.discovery.pid
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn comparable_workspace(workspace: &Path) -> PathBuf {
    workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf())
}

/// Ensure the default TUI/Desktop execution owner exists independently of
/// this client process. Closing or switching a client therefore only detaches
/// it; the server continues provider, tool, workflow, and child-agent work.
async fn ensure_workspace_server(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    yolo: bool,
) -> Result<RuntimeDiscovery> {
    ensure_workspace_server_at(
        paths,
        workspace,
        trust,
        yolo,
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0),
        random_token(),
        None,
    )
    .await
}

async fn ensure_workspace_server_at(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    yolo: bool,
    address: SocketAddr,
    token: String,
    access: Option<haarness_server::auth::ServerAccess>,
) -> Result<RuntimeDiscovery> {
    // Coordinate independent launchers as well as multiple desktop windows.
    // Keep this lease separate from the daemon's lifetime lease.
    let _connection_lease = acquire_connection_lease(paths).await?;
    if let Some(live) = inspect_workspace_server(paths, workspace).await? {
        if let Some(requested) = &access {
            anyhow::ensure!(
                live.discovery.access_fingerprint.as_deref()
                    == Some(requested.fingerprint(&live.discovery.token).as_str()),
                "This project already has a server with different authentication settings. Stop it with serve --stop before changing authentication."
            );
        }
        // Client launchers must not replace a manually configured open/password
        // server with a private daemon merely because the executable changed.
        if access.is_none()
            && live
                .discovery
                .access_fingerprint
                .as_deref()
                .is_some_and(|value| {
                    value
                        != haarness_server::auth::ServerAccess::Private
                            .fingerprint(&live.discovery.token)
                })
        {
            if let Some(reason) = server_compatibility_error(&live.info) {
                anyhow::bail!("Restart the configured server before connecting: {reason}");
            }
            anyhow::ensure!(
                live.info.project_trusted == Some(trust),
                "The configured server uses different project configuration settings. Restart it with the desired --disable-project-config setting before connecting."
            );
            return Ok(live.discovery);
        }
        if let Some(reason) = local_server_compatibility_error(&live).or_else(|| {
            (live.info.project_trusted != Some(trust))
                .then(|| "project configuration setting changed".to_owned())
        }) {
            replace_incompatible_workspace_server(paths, workspace, live, &reason).await?;
        } else {
            return Ok(live.discovery);
        }
    }

    let access = access.unwrap_or_default();
    let executable = std::env::current_exe().context("cannot locate the Haarness executable")?;
    let mut command = ProcessCommand::new(executable);

    command
        .args(paths.root.as_ref().map(|_| "--data-dir"))
        .args(paths.root.as_ref())
        .arg("--workspace")
        .arg(workspace)
        .args((!trust).then_some("--disable-project-config"))
        .env(
            "HAARNESS_DISABLE_PROJECT_CONFIG",
            if trust { "0" } else { "1" },
        )
        .args(yolo.then_some("--yolo"))
        .arg("serve")
        .arg("--host")
        .arg(address.ip().to_string())
        .arg("--port")
        .arg(address.port().to_string())
        .arg("--background-child")
        .args(matches!(access, haarness_server::auth::ServerAccess::Open).then_some("--anonymous"))
        .env("HAARNESS_SERVER_TOKEN", token)
        .env_remove("HAARNESS_SERVER_USERNAME")
        .env_remove("HAARNESS_SERVER_PASSWORD")
        .envs(match &access {
            haarness_server::auth::ServerAccess::Password { username, password } => vec![
                ("HAARNESS_SERVER_USERNAME", username),
                ("HAARNESS_SERVER_PASSWORD", password),
            ],
            _ => vec![],
        })
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .context("cannot start the Haarness server")?;
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(discovery) = discover_workspace_server(paths, workspace).await? {
            return Ok(discovery);
        }
        if let Some(status) = child.try_wait()? {
            // Another launcher may have won the lease while this child was
            // starting. Give its discovery write one final chance to land.
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Some(discovery) = discover_workspace_server(paths, workspace).await? {
                return Ok(discovery);
            }
            anyhow::bail!("Haarness server exited during startup ({status})");
        }
        if Instant::now() >= deadline {
            anyhow::bail!("Haarness server did not become ready within 8 seconds");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn acquire_connection_lease(paths: &Paths) -> Result<fs::File> {
    let file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(paths.runtime.with_extension("connect.lock"))?;
    set_private(&file)?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(file),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    anyhow::bail!(
                        "Another connection to this workspace is still starting; retry shortly"
                    );
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => return Err(error).context("coordinate workspace connection"),
        }
    }
}

fn acquire_server_lease(paths: &Paths) -> Result<fs::File> {
    let file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&paths.server_lock)
        .with_context(|| format!("cannot open {}", paths.server_lock.display()))?;
    file.try_lock_exclusive().with_context(|| {
        format!(
            "another Haarness server is already running or starting ({})",
            paths.server_lock.display()
        )
    })?;
    Ok(file)
}

async fn wait_for_shutdown(shutdown: tokio_util::sync::CancellationToken) -> Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            () = shutdown.cancelled() => {}
            result = tokio::signal::ctrl_c() => result?,
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        tokio::select! {
            () = shutdown.cancelled() => {}
            result = tokio::signal::ctrl_c() => result?,
        }
    }
    Ok(())
}

fn remove_runtime_if_owner(paths: &Paths, pid: u32) {
    if read_runtime(paths).is_ok_and(|runtime| runtime.pid == pid)
        && let Err(error) = fs::remove_file(&paths.runtime)
        && error.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(%error, "could not remove runtime discovery");
    }
}
/// Raw auth.json contents. Entries are kept as JSON values so record types
/// this build does not understand survive read→write round trips untouched.
fn read_auth(paths: &Paths) -> Result<BTreeMap<String, serde_json::Value>> {
    if !paths.auth.exists() {
        return Ok(BTreeMap::new());
    }
    Ok(serde_json::from_slice(&fs::read(&paths.auth)?)?)
}
fn resolved_credentials(value: &BTreeMap<String, serde_json::Value>) -> HashMap<String, String> {
    value
        .iter()
        .filter_map(|(provider, raw)| {
            let credential: Credential = serde_json::from_value(raw.clone()).ok()?;
            credential
                .api_key()
                .map(|key| (provider.clone(), key.to_owned()))
        })
        .collect()
}
#[cfg(test)]
fn write_auth(paths: &Paths, value: &BTreeMap<String, serde_json::Value>) -> Result<()> {
    write_private_json(&paths.auth, value)
}
fn write_private_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    let mut file = options.open(&temporary)?;
    set_private(&file)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}
#[cfg(unix)]
fn set_private(file: &fs::File) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(())
}
#[cfg(not(unix))]
fn set_private(_file: &fs::File) -> Result<()> {
    Ok(())
}
fn random_token() -> String {
    format!("{}{}", Uuid::now_v7().simple(), Uuid::now_v7().simple())
}

#[cfg(test)]
mod tests {
    #[test]
    fn project_configuration_is_default_on_with_an_explicit_opt_out() {
        let default = super::Cli::try_parse_from(["haarness", "run", "hello"]).unwrap();
        assert!(!default.disable_project_config);
        assert!(!default.yolo);
        let disabled =
            super::Cli::try_parse_from(["haarness", "--disable-project-config", "run", "hello"])
                .unwrap();
        assert!(disabled.disable_project_config);
        assert!(!disabled.yolo);
        let legacy = super::Cli::try_parse_from([
            "haarness",
            "--trust-project",
            "--disable-project-config",
            "run",
            "hello",
        ])
        .unwrap();
        assert!(
            legacy.disable_project_config,
            "explicit opt-out wins over legacy opt-in"
        );
    }

    #[test]
    fn automatic_snapshots_follow_project_type_and_explicit_configuration() {
        assert!(super::automatic_snapshots_enabled(None, true));
        assert!(!super::automatic_snapshots_enabled(None, false));
        let mut configuration = super::SnapshotsSection::default();
        assert!(super::automatic_snapshots_enabled(
            Some(&configuration),
            false
        ));
        configuration.enabled = false;
        assert!(!super::automatic_snapshots_enabled(
            Some(&configuration),
            true
        ));
    }

    use std::{ffi::OsString, fs, path::PathBuf, sync::Arc, time::Duration};

    use async_trait::async_trait;
    use clap::Parser;
    use futures::stream;
    use haarness_core::{
        CoreResult, ModelProvider, ProviderEvent, ProviderMessage, ProviderRequest, ProviderStream,
    };
    use haarness_protocol::{
        CreateThreadRequest, GoalActionRequest, GoalStatus, Item, ItemData, ItemId, ModelInfo,
        PTY_CAPABILITY, ProviderInfo, QuestionReply, RequestId, ServerInfo, StartTurnRequest,
        ThreadId, TurnId, WORKSPACE_HISTORY_CAPABILITY,
    };
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio_util::sync::CancellationToken;

    use super::{
        AgentRuntime, AuthCommand, BTreeMap, Cli, Command, Credential, FileConfig,
        LiveWorkspaceServer, McpCommand, McpConfigureRequest, Paths, ProviderSection, RunFormat,
        RuntimeDiscovery, SessionCommand, assistant_messages_for_turn, configured_mcp,
        current_executable_fingerprint, finish_goal_run, goal_idle_timeout_from, load_output_style,
        local_server_compatibility_error, mcp_config_for_display,
        migrate_legacy_workspace_database, normalize_natural_language_args, parse_mcp_key_values,
        qualified_run_model, read_mcp_entry, replace_incompatible_workspace_server,
        resolved_credentials, select_run_thread, server_compatibility_error, set_goal_for_run,
        valid_provider_url, wait_for_goal_with_timeout, write_mcp_entry, write_provider_entry,
        write_runtime,
    };

    #[test]
    fn output_style_loads_builtin_and_trusted_project_override() {
        let root =
            std::env::temp_dir().join(format!("haarness-output-style-{}", uuid::Uuid::now_v7()));
        let config = root.join("config");
        let workspace = root.join("workspace");
        fs::create_dir_all(workspace.join(".haarness/output-styles")).unwrap();
        fs::write(
            workspace.join(".haarness/output-styles/review-ready.md"),
            "Project-specific response guidance.",
        )
        .unwrap();
        let builtin = load_output_style(&config, &workspace, false, Some("concise"))
            .unwrap()
            .unwrap();
        assert_eq!(builtin.id, "concise");
        let custom = load_output_style(&config, &workspace, true, Some("review-ready"))
            .unwrap()
            .unwrap();
        assert_eq!(custom.instructions, "Project-specific response guidance.");
        assert!(load_output_style(&config, &workspace, true, Some("../escape")).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    struct CriteriaQuestionProvider;

    #[async_trait]
    impl ModelProvider for CriteriaQuestionProvider {
        fn id(&self) -> &str {
            "question-test"
        }

        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                id: self.id().into(),
                name: "Question test".into(),
                authenticated: true,
                models: vec![ModelInfo {
                    id: "question-test-1".into(),
                    display_name: "Question test".into(),
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
        ) -> CoreResult<ProviderStream> {
            let criteria_approved = request.messages.iter().any(|message| {
                matches!(
                    message,
                    ProviderMessage::ToolResult { name, .. } if name == "propose_goal_criteria"
                )
            });
            let goal_updated = request.messages.iter().any(|message| {
                matches!(
                    message,
                    ProviderMessage::ToolResult { name, .. } if name == "update_goal"
                )
            });
            let events = if goal_updated {
                vec![
                    Ok(ProviderEvent::TextDelta(
                        "The goal is complete after the non-interactive fallback.".into(),
                    )),
                    Ok(ProviderEvent::Finished {
                        reason: "done".into(),
                    }),
                ]
            } else if criteria_approved {
                vec![Ok(ProviderEvent::ToolCall {
                    call_id: "complete-1".into(),
                    name: "update_goal".into(),
                    arguments: serde_json::json!({
                        "status": "complete",
                        "reason": "the implementation and focused tests are verified",
                        "next_step_question": {
                            "title": "What next?",
                            "edit": {"label": "Edit", "description": "Refine this goal."},
                            "new": {"label": "New", "description": "Start another goal."},
                            "clear": {"label": "Clear", "description": "Remove this goal."},
                            "dismiss": {"label": "Done", "description": "Close this dialog."}
                        },
                        "criteria_evidence": [
                            {"index": 1, "evidence": "implementation verified"},
                            {"index": 2, "evidence": "focused tests passed"}
                        ]
                    }),
                    provider_metadata: serde_json::Value::Null,
                })]
            } else {
                vec![Ok(ProviderEvent::ToolCall {
                    call_id: "criteria-1".into(),
                    name: "propose_goal_criteria".into(),
                    arguments: serde_json::json!({
                        "criteria": ["The change is implemented", "Focused tests pass"],
                        "prompt": "Approve these criteria?",
                        "label": "Completion criteria"
                    }),
                    provider_metadata: serde_json::Value::Null,
                })]
            };
            Ok(Box::pin(stream::iter(events)))
        }
    }

    async fn goal_test_runtime() -> (Arc<AgentRuntime>, ThreadId, PathBuf) {
        let root = std::env::temp_dir().join(format!("haarness-cli-goal-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let store = Arc::new(haarness_store::SqliteStore::open(root.join("haarness.db")).unwrap());
        let providers = Arc::new(haarness_core::StaticProviderRegistry::new(Vec::<
            Arc<dyn haarness_core::ModelProvider>,
        >::new()));
        let runtime = Arc::new(AgentRuntime::new(
            store,
            providers,
            Arc::new(haarness_core::EmptyToolRegistry),
            haarness_core::EventBus::new(32),
        ));
        let thread = runtime
            .create_thread(CreateThreadRequest {
                schedule: None,
                title: Some("goal watcher test".into()),
                parent_id: None,
                agent: Some("build".into()),
                provider: Some("test".into()),
                model: Some("test".into()),
                ultracode: None,
            })
            .await
            .unwrap();
        (runtime, thread.id, root)
    }

    async fn question_test_runtime() -> (Arc<AgentRuntime>, ThreadId, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "haarness-cli-goal-question-{}",
            uuid::Uuid::now_v7()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Arc::new(haarness_store::SqliteStore::open(root.join("haarness.db")).unwrap());
        let provider: Arc<dyn ModelProvider> = Arc::new(CriteriaQuestionProvider);
        let providers = Arc::new(haarness_core::StaticProviderRegistry::new(vec![provider]));
        let runtime = Arc::new(AgentRuntime::new(
            store,
            providers,
            // Exercise the production tool catalog: runtime-owned goal calls
            // must be present in the advertised schema snapshot before the
            // execution-authority check will accept them.
            Arc::new(haarness_server::NativeToolRegistry::new(root.clone())),
            haarness_core::EventBus::new(32),
        ));
        let thread = runtime
            .create_thread(CreateThreadRequest {
                schedule: None,
                title: Some("goal question test".into()),
                parent_id: None,
                agent: Some("build".into()),
                provider: Some("question-test".into()),
                model: Some("question-test-1".into()),
                ultracode: None,
            })
            .await
            .unwrap();
        (runtime, thread.id, root)
    }

    #[test]
    fn worktree_management_and_legacy_aliases_are_not_treated_as_prompts() {
        for name in ["worktree", "worktrees", "workspace", "workspaces"] {
            let args = normalize_natural_language_args(
                ["haarness", name, "list", "--json"].map(OsString::from),
            );
            let cli = Cli::try_parse_from(args).unwrap();
            assert!(matches!(cli.command, Some(Command::Workspace { .. })));
        }
    }

    #[test]
    fn run_accepts_the_ultracode_flag() {
        let cli = Cli::try_parse_from([
            "haarness",
            "run",
            "--ultracode",
            "audit",
            "the",
            "workspace",
        ])
        .unwrap();
        let Some(Command::Run(args)) = cli.command else {
            panic!("expected run command");
        };
        assert!(args.ultracode);
        assert_eq!(args.prompt.join(" "), "audit the workspace");
    }

    #[test]
    fn natural_language_argv_is_rewritten_to_a_one_shot_run() {
        let normalized = normalize_natural_language_args(
            [
                "haarness",
                "audit",
                "the",
                "workspace",
                "--model",
                "openai/gpt-5.6-sol",
            ]
            .map(OsString::from),
        );
        let cli = Cli::try_parse_from(normalized).expect("natural language CLI");
        let Some(Command::Run(run_args)) = cli.command else {
            panic!("expected implicit run command");
        };
        assert_eq!(run_args.prompt.join(" "), "audit the workspace");
        assert_eq!(run_args.model, "openai/gpt-5.6-sol");
        assert_eq!(
            qualified_run_model(&run_args.provider, &run_args.model).unwrap(),
            ("openai".into(), "gpt-5.6-sol".into())
        );
    }

    #[test]
    fn natural_language_rewrite_preserves_commands_paths_and_options() {
        for argv in [
            vec!["haarness", "models"],
            vec!["haarness", "./workspace"],
            vec!["haarness", "--help"],
        ] {
            let normalized = normalize_natural_language_args(argv.iter().map(OsString::from));
            assert_ne!(
                normalized.get(1).and_then(|value| value.to_str()),
                Some("run")
            );
        }
    }

    #[test]
    fn run_exposes_haarness_style_shortcuts_and_background_controls() {
        let session = uuid::Uuid::now_v7();
        let cli = Cli::try_parse_from([
            "haarness",
            "run",
            "-s",
            &session.to_string(),
            "-f",
            "image.png",
            "-m",
            "openai/gpt-5.6-sol",
            "--title",
            "Audit",
            "--format",
            "json",
            "--wait-background",
            "--background-timeout-ms",
            "5000",
            "--thinking",
            "inspect",
        ])
        .expect("run parity options");
        let Some(Command::Run(args)) = cli.command else {
            panic!("expected run command");
        };
        assert_eq!(cli.thread, Some(session));
        assert_eq!(args.files, vec![PathBuf::from("image.png")]);
        assert_eq!(args.title.as_deref(), Some("Audit"));
        assert_eq!(args.format, RunFormat::Json);
        assert!(args.wait_background);
        assert_eq!(args.background_timeout_ms, Some(5_000));
        assert!(args.thinking);
        assert!(!args.no_thinking);
        assert!(args.show_thinking());

        let cli = Cli::try_parse_from(["haarness", "run", "inspect"])
            .expect("thinking is visible by default");
        let Some(Command::Run(args)) = cli.command else {
            panic!("expected run command");
        };
        assert!(args.show_thinking());

        let cli = Cli::try_parse_from(["haarness", "run", "--no-thinking", "inspect"])
            .expect("thinking can be explicitly hidden");
        let Some(Command::Run(args)) = cli.command else {
            panic!("expected run command");
        };
        assert!(!args.thinking);
        assert!(args.no_thinking);
        assert!(!args.show_thinking());
    }

    #[test]
    fn mcp_cli_exposes_aliases_environment_headers_and_lifecycle_controls() {
        let cli = Cli::try_parse_from(["haarness", "mcp", "ls"]).expect("mcp ls alias");
        assert!(matches!(
            cli.command,
            Some(Command::Mcp {
                command: McpCommand::List
            })
        ));

        let cli = Cli::try_parse_from([
            "haarness",
            "mcp",
            "add",
            "fixture",
            "--command",
            "server",
            "--arg",
            "--stdio",
            "--env",
            "TOKEN=${TOKEN}",
            "--disabled",
            "--project",
        ])
        .expect("stdio MCP options");
        let Some(Command::Mcp {
            command:
                McpCommand::Add {
                    args,
                    env,
                    disabled,
                    project,
                    ..
                },
        }) = cli.command
        else {
            panic!("expected MCP add command");
        };
        assert_eq!(args, vec!["--stdio"]);
        assert_eq!(env, vec!["TOKEN=${TOKEN}"]);
        assert!(disabled);
        assert!(project);

        let cli = Cli::try_parse_from(["haarness", "mcp", "rm", "fixture"]).expect("mcp rm alias");
        assert!(matches!(
            cli.command,
            Some(Command::Mcp {
                command: McpCommand::Remove { .. }
            })
        ));
    }

    #[test]
    fn mcp_config_round_trip_preserves_values_but_redacts_default_output() {
        let root = std::env::temp_dir().join(format!("haarness-mcp-cli-{}", uuid::Uuid::now_v7()));
        let path = root.join("config.toml");
        let entry = McpConfigureRequest {
            transport: "streamable_http".into(),
            command: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            url: Some("https://example.test/mcp".into()),
            headers: BTreeMap::from([
                ("Authorization".into(), "Bearer ${TOKEN}".into()),
                ("X-Tenant".into(), "fixture".into()),
            ]),
            headers_helper: None,
            timeout_ms: None,
            oauth: Some(true),
            oauth_client_id: Some("fixture-client".into()),
            oauth_scopes: vec!["mcp:read".into(), "mcp:write".into()],
            enabled: false,
        };
        write_mcp_entry(&path, "fixture", Some(&entry)).unwrap();
        let restored = read_mcp_entry(&path, "fixture").unwrap();
        assert_eq!(restored.headers, entry.headers);
        assert_eq!(restored.oauth, Some(true));
        assert_eq!(restored.oauth_client_id.as_deref(), Some("fixture-client"));
        assert_eq!(restored.oauth_scopes, entry.oauth_scopes);

        let redacted = mcp_config_for_display(&entry, false).unwrap();
        assert_eq!(redacted["headers"]["Authorization"], "<redacted>");
        let explicit = mcp_config_for_display(&entry, true).unwrap();
        assert_eq!(explicit["headers"]["Authorization"], "Bearer ${TOKEN}");

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mcp_key_values_reject_duplicates_and_malformed_entries() {
        assert!(parse_mcp_key_values(&["TOKEN".into()], "environment variable").is_err());
        assert!(
            parse_mcp_key_values(
                &["TOKEN=a".into(), "TOKEN=b".into()],
                "environment variable"
            )
            .is_err()
        );
    }

    #[test]
    fn cli_exposes_sessions_providers_and_agents() {
        let cli = Cli::try_parse_from(["haarness", "session", "ls", "--all", "--json"])
            .expect("session aliases");
        assert!(matches!(
            cli.command,
            Some(Command::Sessions {
                command: SessionCommand::List {
                    all: true,
                    json: true
                }
            })
        ));
        let session = uuid::Uuid::now_v7();
        let cli = Cli::try_parse_from([
            "haarness",
            "sessions",
            "export",
            &session.to_string(),
            "--sanitize",
            "--output",
            "archive.json",
            "--force",
        ])
        .expect("session archive export options");
        assert!(matches!(
            cli.command,
            Some(Command::Sessions {
                command: SessionCommand::Export {
                    sanitize: true,
                    force: true,
                    ..
                }
            })
        ));
        assert!(
            Cli::try_parse_from([
                "haarness",
                "sessions",
                "export",
                &session.to_string(),
                "--force"
            ])
            .is_err(),
            "--force only makes sense with an output file"
        );
        assert!(
            Cli::try_parse_from(["haarness", "sessions", "import", "archive.json", "--json"])
                .is_ok()
        );
        assert!(Cli::try_parse_from(["haarness", "providers", "--verbose"]).is_ok());
        assert!(
            Cli::try_parse_from([
                "haarness",
                "provider",
                "add",
                "company-gateway",
                "--url",
                "https://models.example.test/v1",
                "--protocol",
                "anthropic-messages",
                "--model",
                "company-sonnet",
                "--header",
                "X-Tenant=engineering",
            ])
            .is_ok()
        );
        assert!(Cli::try_parse_from(["haarness", "agents", "--all", "--json"]).is_ok());
    }

    #[test]
    fn cli_keeps_side_questions_without_team_commands() {
        let thread = uuid::Uuid::now_v7().to_string();
        assert!(
            Cli::try_parse_from(["haarness", "btw", &thread, "what is happening?", "--json"])
                .is_ok()
        );
        assert!(Cli::try_parse_from(["haarness", "team", "list"]).is_err());
    }

    #[test]
    fn provider_config_management_round_trips_protocol_without_credentials() {
        let root =
            std::env::temp_dir().join(format!("haarness-provider-cli-{}", uuid::Uuid::now_v7()));
        let path = root.join("config.toml");
        let section = ProviderSection {
            protocol: "gemini-generate-content".into(),
            name: Some("Company Gateway".into()),
            base_url: Some("https://models.example.test/gemini/v1beta".into()),
            api_key_env: Some("COMPANY_MODELS_KEY".into()),
            headers: BTreeMap::from([("X-Tenant".into(), "engineering".into())]),
            models: vec!["company-flash".into()],
            ..ProviderSection::default()
        };
        write_provider_entry(&path, "company-gateway", Some(&section)).unwrap();

        let config: FileConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let saved = config.providers.get("company-gateway").unwrap();
        assert_eq!(saved.protocol, "gemini-generate-content");
        assert_eq!(saved.api_key_env.as_deref(), Some("COMPANY_MODELS_KEY"));
        assert_eq!(saved.headers.get("X-Tenant").unwrap(), "engineering");
        assert_eq!(saved.models, ["company-flash"]);
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("api_key =")
        );

        write_provider_entry(&path, "company-gateway", None).unwrap();
        let config: FileConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(config.providers.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn provider_urls_require_tls_except_for_exact_loopback_hosts() {
        assert!(valid_provider_url("https://models.example.test/v1"));
        assert!(valid_provider_url("http://localhost:11434/v1"));
        assert!(valid_provider_url("http://127.0.0.1:8000/v1"));
        assert!(valid_provider_url("http://[::1]:1234/v1"));
        assert!(!valid_provider_url("http://models.example.test/v1"));
        assert!(!valid_provider_url("http://localhost.evil.test/v1"));
        assert!(!valid_provider_url("https://user@example.test/v1"));
    }

    #[test]
    fn server_password_options_are_shared_by_all_connection_entrypoints() {
        for arguments in [
            vec![
                "haarness",
                "serve",
                "--username",
                "alex",
                "--password",
                "secret",
            ],
            vec![
                "haarness",
                "attach",
                "http://localhost:4096",
                "--username",
                "alex",
                "--password",
                "secret",
            ],
            vec![
                "haarness",
                "tui",
                "--attach",
                "http://localhost:4096",
                "--username",
                "alex",
                "--password",
                "secret",
            ],
            vec![
                "haarness",
                "run",
                "--attach",
                "http://localhost:4096",
                "--username",
                "alex",
                "--password",
                "secret",
                "hello",
            ],
        ] {
            let parsed = Cli::try_parse_from(arguments).unwrap();
            let authentication = match parsed.command.unwrap() {
                Command::Serve(args) => args.authentication,
                Command::Attach(args) => args.authentication,
                Command::Tui(args) => args.authentication,
                Command::Run(args) => args.authentication,
                _ => panic!("unexpected command"),
            };
            assert_eq!(authentication.username.as_deref(), Some("alex"));
            assert_eq!(authentication.password.as_deref(), Some("secret"));
        }
    }

    #[test]
    fn auth_cli_exposes_browser_headless_api_and_logout_flows() {
        for method in ["browser", "headless", "api"] {
            let cli =
                Cli::try_parse_from(["haarness", "auth", "login", "openai", "--method", method])
                    .unwrap();
            assert!(matches!(
                cli.command,
                Some(Command::Auth {
                    command: AuthCommand::Login {
                        provider,
                        method: Some(_)
                    }
                }) if provider == "openai"
            ));
        }
        for method in ["browser", "headless", "api"] {
            assert!(
                Cli::try_parse_from(["haarness", "auth", "login", "xai", "--method", method])
                    .is_ok()
            );
        }
        assert!(Cli::try_parse_from(["haarness", "auth", "logout", "openai"]).is_ok());
    }

    #[test]
    fn serve_exposes_persistent_lifecycle_controls() {
        let daemon = Cli::try_parse_from(["haarness", "serve", "--daemon"]).unwrap();
        let Some(Command::Serve(args)) = daemon.command else {
            panic!("expected serve command");
        };
        assert!(args.daemon);
        assert!(!args.stop);

        let stop = Cli::try_parse_from(["haarness", "serve", "--stop"]).unwrap();
        let Some(Command::Serve(args)) = stop.command else {
            panic!("expected serve command");
        };
        assert!(args.stop);
        assert!(!args.daemon);
        assert!(
            Cli::try_parse_from(["haarness", "serve", "--daemon", "--stop"]).is_err(),
            "start and stop must remain mutually exclusive"
        );
    }

    #[test]
    fn attach_is_first_class_for_tui_and_headless_runs() {
        let attach = Cli::try_parse_from([
            "haarness",
            "--session",
            "018f0000-0000-7000-8000-000000000001",
            "attach",
            "http://127.0.0.1:4096",
            "--token",
            "secret",
            "--fork",
        ])
        .unwrap();
        let Some(Command::Attach(args)) = attach.command else {
            panic!("expected attach command");
        };
        assert_eq!(args.url, "http://127.0.0.1:4096");
        assert_eq!(args.token.as_deref(), Some("secret"));
        assert!(args.fork);

        let run = Cli::try_parse_from([
            "haarness",
            "run",
            "hello",
            "--attach",
            "https://server.example",
            "--token",
            "secret",
        ])
        .unwrap();
        let Some(Command::Run(args)) = run.command else {
            panic!("expected run command");
        };
        assert_eq!(args.attach.as_deref(), Some("https://server.example"));
        assert_eq!(args.token.as_deref(), Some("secret"));

        let tui = Cli::try_parse_from([
            "haarness",
            "tui",
            "--attach",
            "http://localhost:4096",
            "--token",
            "secret",
        ])
        .unwrap();
        let Some(Command::Tui(args)) = tui.command else {
            panic!("expected tui command");
        };
        assert_eq!(args.connect.as_deref(), Some("http://localhost:4096"));
    }

    #[test]
    fn legacy_runtime_discovery_remains_readable() {
        let discovery: RuntimeDiscovery = serde_json::from_value(serde_json::json!({
            "url": "http://127.0.0.1:1234",
            "token": "test",
            "pid": 42
        }))
        .unwrap();
        assert_eq!(discovery.pid, 42);
        assert!(discovery.workspace.is_none());
        assert!(discovery.executable_fingerprint.is_none());
    }

    fn fixture_server_info(capabilities: Vec<String>) -> ServerInfo {
        ServerInfo {
            name: "haarness".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            schema_version: haarness_protocol::SCHEMA_VERSION.into(),
            server_instance_id: uuid::Uuid::now_v7(),
            event_cursor: 0,
            capabilities,
            workspace: None,
            git_branch: None,
            default_provider: None,
            default_model: None,
            default_agent: None,
            project_trusted: None,
        }
    }

    #[test]
    fn workspace_server_compatibility_requires_history_api_and_current_schema() {
        let mut info = fixture_server_info(Vec::new());
        let error = server_compatibility_error(&info).unwrap();
        assert!(error.contains(WORKSPACE_HISTORY_CAPABILITY), "{error}");

        info.capabilities.push(WORKSPACE_HISTORY_CAPABILITY.into());
        assert!(server_compatibility_error(&info).is_some());
        info.capabilities.push(PTY_CAPABILITY.into());
        assert!(server_compatibility_error(&info).is_none());

        info.schema_version = "future".into();
        let error = server_compatibility_error(&info).unwrap();
        assert!(error.contains("API schema future"), "{error}");
    }

    #[test]
    fn local_server_compatibility_requires_the_current_executable() {
        let current = current_executable_fingerprint().unwrap();
        let mut live = LiveWorkspaceServer {
            discovery: RuntimeDiscovery {
                url: "http://127.0.0.1:1234".into(),
                token: "test".into(),
                pid: 42,
                workspace: None,
                executable_fingerprint: Some(current),
                access_fingerprint: None,
            },
            info: fixture_server_info(vec![
                WORKSPACE_HISTORY_CAPABILITY.into(),
                PTY_CAPABILITY.into(),
            ]),
        };
        assert!(local_server_compatibility_error(&live).is_none());

        live.discovery.executable_fingerprint = Some("stale-build".into());
        assert_eq!(
            local_server_compatibility_error(&live).as_deref(),
            Some("server executable differs from this client build")
        );
        live.discovery.executable_fingerprint = None;
        assert!(local_server_compatibility_error(&live).is_some());
    }

    #[tokio::test]
    async fn incompatible_idle_workspace_server_is_stopped_before_replacement() {
        let root = std::env::temp_dir().join(format!(
            "haarness-stale-server-test-{}",
            uuid::Uuid::now_v7()
        ));
        let workspace = root.join("workspace");
        let state_root = root.join("state");
        fs::create_dir_all(&workspace).unwrap();
        let paths = Paths::new(Some(&state_root)).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let discovery = RuntimeDiscovery {
            url: format!("http://{address}"),
            token: "test-token".into(),
            pid: 4242,
            workspace: Some(workspace.display().to_string()),
            executable_fingerprint: None,
            access_fingerprint: None,
        };
        write_runtime(&paths, &discovery).unwrap();
        let runtime_path = paths.runtime.clone();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                loop {
                    let read = socket.read(&mut chunk).await.unwrap();
                    request.extend_from_slice(&chunk[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8(request).unwrap();
                let (status, body) = if request.starts_with("GET /v1/turns/active ") {
                    ("200 OK", "[]")
                } else if request.starts_with("POST /v1/shutdown ") {
                    ("202 Accepted", "")
                } else {
                    panic!("unexpected stale-server request: {request}");
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
                let shutdown = request.starts_with("POST /v1/shutdown ");
                requests.push(request);
                if shutdown {
                    fs::remove_file(&runtime_path).unwrap();
                }
            }
            requests
        });

        let info = fixture_server_info(Vec::new());
        replace_incompatible_workspace_server(
            &paths,
            &workspace,
            LiveWorkspaceServer { discovery, info },
            "missing history capability",
        )
        .await
        .unwrap();

        assert!(!paths.runtime.exists());
        let requests = server.await.unwrap();
        assert!(requests[0].starts_with("GET /v1/turns/active "));
        assert!(requests[1].starts_with("POST /v1/shutdown "));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn run_accepts_a_goal_objective() {
        let cli = Cli::try_parse_from([
            "haarness",
            "run",
            "--goal",
            "finish",
            "the",
            "migration",
            "--provider",
            "anthropic",
            "--model",
            "claude-sonnet-4-6",
        ])
        .unwrap();
        let Some(Command::Run(args)) = cli.command else {
            panic!("expected run command");
        };
        assert!(args.goal);
        assert_eq!(args.provider, "anthropic");
        assert_eq!(args.model, "claude-sonnet-4-6");
        assert_eq!(args.prompt.join(" "), "finish the migration");
    }

    #[test]
    fn run_accepts_session_fork_for_a_multi_token_goal() {
        let session = uuid::Uuid::now_v7();
        let cli = Cli::try_parse_from([
            "haarness",
            "run",
            "--session",
            &session.to_string(),
            "--goal",
            "finish",
            "the",
            "migration",
            "--fork",
        ])
        .unwrap();
        let Some(Command::Run(args)) = cli.command else {
            panic!("expected run command");
        };
        assert_eq!(cli.thread, Some(session));
        assert!(args.fork);
        assert_eq!(args.prompt.join(" "), "finish the migration");
    }

    #[test]
    fn run_goal_rejects_other_orchestration_modes() {
        for conflicting in [["--ultracode", ""], ["--workflow", "release"]] {
            let mut argv = vec!["haarness", "run", "--goal", "finish the migration"];
            argv.push(conflicting[0]);
            if !conflicting[1].is_empty() {
                argv.push(conflicting[1]);
            }
            assert!(
                Cli::try_parse_from(argv).is_err(),
                "{} should conflict with --goal",
                conflicting[0]
            );
        }
    }

    #[test]
    fn goal_idle_watchdog_is_bounded_and_rejects_invalid_overrides() {
        assert_eq!(goal_idle_timeout_from(Some("1250")).as_millis(), 1_250);
        assert_eq!(goal_idle_timeout_from(Some("0")).as_secs(), 45);
        assert_eq!(goal_idle_timeout_from(Some("invalid")).as_secs(), 45);
        assert_eq!(goal_idle_timeout_from(None).as_secs(), 45);
    }

    #[test]
    fn goal_run_only_succeeds_for_complete_state() {
        let mut state = haarness_core::new_goal_state("finish migration", None).unwrap();
        state.status = GoalStatus::Complete;
        assert!(finish_goal_run(&state).is_ok());

        for status in [
            GoalStatus::Paused,
            GoalStatus::Blocked,
            GoalStatus::UsageLimited,
            GoalStatus::BudgetLimited,
            GoalStatus::Cleared,
        ] {
            state.status = status;
            assert!(
                finish_goal_run(&state).is_err(),
                "{status:?} must be a non-successful goal exit"
            );
        }
    }

    #[tokio::test]
    async fn goal_watcher_follows_updates_until_complete() {
        let (runtime, thread_id, root) = goal_test_runtime().await;
        let subscription = runtime.events().subscribe(Some(runtime.events().cursor()));
        runtime
            .goal_action(
                thread_id,
                GoalActionRequest::Set {
                    goal: "finish migration".into(),
                    token_budget: None,
                },
            )
            .await
            .unwrap();
        let completing = Arc::clone(&runtime);
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            completing
                .goal_action(thread_id, GoalActionRequest::Complete)
                .await
                .unwrap();
        });

        wait_for_goal_with_timeout(
            runtime.as_ref(),
            thread_id,
            subscription,
            Duration::from_millis(250),
        )
        .await
        .unwrap();
        task.await.unwrap();
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn goal_watcher_bounds_idle_active_goals() {
        let (runtime, thread_id, root) = goal_test_runtime().await;
        let subscription = runtime.events().subscribe(Some(runtime.events().cursor()));
        runtime
            .goal_action(
                thread_id,
                GoalActionRequest::Set {
                    goal: "finish migration".into(),
                    token_budget: None,
                },
            )
            .await
            .unwrap();

        let error = wait_for_goal_with_timeout(
            runtime.as_ref(),
            thread_id,
            subscription,
            Duration::from_millis(20),
        )
        .await
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("without a goal update while idle")
        );
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn goal_watcher_resolves_criteria_questions_without_hanging() {
        let (runtime, thread_id, root) = question_test_runtime().await;
        let subscription = runtime.events().subscribe(Some(runtime.events().cursor()));
        runtime
            .goal_action(
                thread_id,
                GoalActionRequest::Set {
                    goal: "finish migration".into(),
                    token_budget: None,
                },
            )
            .await
            .unwrap();
        runtime
            .start_turn(
                thread_id,
                StartTurnRequest {
                    request_id: RequestId::new(),
                    text: "finish migration".into(),
                    attachments: Vec::new(),
                    synthetic: false,
                    auto_approve: false,
                    agent: Some("build".into()),
                    provider: Some("question-test".into()),
                    model: Some("question-test-1".into()),
                    reasoning_effort: None,
                    tools: None,
                    command: None,
                },
            )
            .await
            .unwrap();

        tokio::time::timeout(
            Duration::from_secs(1),
            wait_for_goal_with_timeout(
                runtime.as_ref(),
                thread_id,
                subscription,
                Duration::from_millis(100),
            ),
        )
        .await
        .expect("the goal watcher hung on an interactive question")
        .unwrap();
        let criteria_result = runtime
            .items(thread_id, None)
            .await
            .unwrap()
            .into_iter()
            .find_map(|item| match item.data {
                ItemData::ToolResult {
                    name,
                    output,
                    is_error,
                    ..
                } if name == "propose_goal_criteria" => Some((output, is_error)),
                _ => None,
            })
            .expect("criteria tool result");
        assert!(
            !criteria_result.1,
            "headless criteria fallback must approve cleanly"
        );
        assert!(criteria_result.0.contains("The change is implemented"));
        assert!(criteria_result.0.contains("Focused tests pass"));
        let state = runtime.goal_state(thread_id).await.unwrap().unwrap();
        assert_eq!(state.status, GoalStatus::Complete);
        assert_eq!(
            state
                .criteria
                .iter()
                .map(|criterion| criterion.text.as_str())
                .collect::<Vec<_>>(),
            ["The change is implemented", "Focused tests pass"]
        );
        assert!(
            !state
                .criteria
                .iter()
                .any(|criterion| criterion.text.contains("rejected")),
            "the refusal must never become a completion criterion"
        );
        assert!(runtime.pending_questions().is_empty());
        tokio::time::timeout(Duration::from_secs(1), async {
            while runtime.active_turn(thread_id).await.unwrap().is_some() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("goal turn did not settle after completion");
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reused_run_output_contains_only_the_new_turn() {
        let thread_id = ThreadId::new();
        let old_turn = TurnId::new();
        let current_turn = TurnId::new();
        let item = |sequence, turn_id, data| Item {
            id: ItemId::new(),
            thread_id,
            turn_id: Some(turn_id),
            sequence,
            data,
            created_at: chrono::Utc::now(),
        };
        let messages = assistant_messages_for_turn(
            vec![
                item(
                    1,
                    old_turn,
                    ItemData::AssistantMessage {
                        text: "historical answer".into(),
                    },
                ),
                item(
                    2,
                    current_turn,
                    ItemData::UserMessage {
                        text: "new objective".into(),
                        attachments: Vec::new(),
                        synthetic: false,
                        queued: false,
                    },
                ),
                item(
                    3,
                    current_turn,
                    ItemData::AssistantMessage {
                        text: "new answer".into(),
                    },
                ),
            ],
            current_turn,
        );
        assert_eq!(messages, ["new answer"]);
    }

    #[tokio::test]
    async fn run_thread_selection_honors_session_continue_and_fork() {
        let (runtime, thread_id, root) = goal_test_runtime().await;
        let request = || CreateThreadRequest {
            schedule: None,
            title: Some("new CLI run".into()),
            parent_id: None,
            agent: Some("build".into()),
            provider: Some("test".into()),
            model: Some("test".into()),
            ultracode: None,
        };

        let explicit =
            select_run_thread(runtime.as_ref(), Some(thread_id.0), false, false, request())
                .await
                .unwrap();
        assert_eq!(explicit.id, thread_id);

        let continued = select_run_thread(runtime.as_ref(), None, true, false, request())
            .await
            .unwrap();
        assert_eq!(continued.id, thread_id);

        let forked = select_run_thread(runtime.as_ref(), Some(thread_id.0), false, true, request())
            .await
            .unwrap();
        assert_ne!(forked.id, thread_id);
        assert_eq!(forked.parent_id, Some(thread_id));

        let error = select_run_thread(runtime.as_ref(), None, false, true, request())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("--fork requires"));
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn busy_reused_session_does_not_replace_its_goal() {
        let (runtime, thread_id, root) = question_test_runtime().await;
        runtime
            .goal_action(
                thread_id,
                GoalActionRequest::Set {
                    goal: "existing goal".into(),
                    token_budget: None,
                },
            )
            .await
            .unwrap();
        let turn = runtime
            .start_turn(
                thread_id,
                StartTurnRequest {
                    request_id: RequestId::new(),
                    text: "keep working".into(),
                    attachments: Vec::new(),
                    synthetic: false,
                    auto_approve: false,
                    agent: Some("build".into()),
                    provider: Some("question-test".into()),
                    model: Some("question-test-1".into()),
                    reasoning_effort: None,
                    tools: None,
                    command: None,
                },
            )
            .await
            .unwrap();
        let question = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Some(question) = runtime.pending_questions().into_iter().next() {
                    break question;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("provider never asked the criteria question");

        let error = set_goal_for_run(runtime.as_ref(), thread_id, "replacement goal".into())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("current turn is still running"));
        assert_eq!(
            runtime.goal_state(thread_id).await.unwrap().unwrap().goal,
            "existing goal"
        );

        runtime
            .reply_question(
                &question.question_id,
                QuestionReply {
                    answer: "[]".into(),
                },
            )
            .unwrap();
        runtime
            .goal_action(thread_id, GoalActionRequest::Clear)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if !matches!(
                    runtime.turn(turn.id).await.unwrap().status,
                    haarness_protocol::TurnStatus::Queued | haarness_protocol::TurnStatus::Running
                ) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("turn did not stop after clearing the goal");
        drop(runtime);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn workflow_commands_accept_model_defaults() {
        let cli = Cli::try_parse_from([
            "haarness",
            "workflow",
            "run",
            "release",
            "--agent",
            "review",
            "--model",
            "anthropic/claude-opus-4-1",
            "--variant",
            "high",
        ])
        .unwrap();
        let Some(Command::Workflow {
            command:
                super::WorkflowCommand::Run {
                    agent,
                    model,
                    variant,
                    ..
                },
        }) = cli.command
        else {
            panic!("expected workflow run command");
        };
        assert_eq!(agent.as_deref(), Some("review"));
        assert_eq!(model.as_deref(), Some("anthropic/claude-opus-4-1"));
        assert_eq!(variant.as_deref(), Some("high"));
    }

    #[test]
    fn run_accepts_direct_workflow_and_repeated_arguments() {
        let cli = Cli::try_parse_from([
            "haarness",
            "run",
            "summarize",
            "changes",
            "--workflow",
            "release",
            "--workflow-arg",
            "base=main",
            "--workflow-arg",
            "dryRun=true",
            "--variant",
            "high",
        ])
        .unwrap();
        let Some(Command::Run(args)) = cli.command else {
            panic!("expected run command");
        };
        assert_eq!(args.workflow.as_deref(), Some("release"));
        assert_eq!(args.workflow_args, ["base=main", "dryRun=true"]);
        assert_eq!(args.variant.as_deref(), Some("high"));
        assert_eq!(args.prompt.join(" "), "summarize changes");
    }

    #[tokio::test]
    async fn mcp_cli_sees_configured_servers() {
        let root = std::env::temp_dir().join(format!("haarness-mcp-cli-{}", uuid::Uuid::now_v7()));
        let config_dir = root.join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.toml"),
            "[mcp.demo]\ntransport = \"stdio\"\ncommand = \"true\"\nenabled = false\n",
        )
        .unwrap();
        let paths = Paths {
            root: Some(root.clone()),
            data: root.join("data"),
            cache: root.join("cache"),
            database: root.join("data/haarness.db"),
            auth: config_dir.join("auth.json"),
            runtime: root.join("cache/runtime.json"),
            server_lock: root.join("cache/server.lock"),
            skills: root.join("data/skills"),
            config: config_dir.join("config.toml"),
            workspace_id: None,
        };

        let registry = configured_mcp(&paths, &root, false).await.unwrap();
        let servers = haarness_core::McpRegistry::list(registry.as_ref())
            .await
            .unwrap();
        assert!(
            servers
                .iter()
                .any(|server| server.name == "demo" && server.status == "disabled"),
            "expected the configured demo server, got {servers:?}"
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn typed_and_legacy_credentials_are_both_resolved() {
        let credentials: BTreeMap<String, serde_json::Value> = serde_json::from_str(
            r#"{
                "openai": {"type": "api", "key": "typed"},
                "anthropic": "legacy",
                "chatgpt": {"type": "oauth", "access": "a", "refresh": "r", "expires": 0}
            }"#,
        )
        .unwrap();

        let resolved = resolved_credentials(&credentials);
        assert_eq!(resolved.get("openai").map(String::as_str), Some("typed"));
        assert_eq!(
            resolved.get("anthropic").map(String::as_str),
            Some("legacy")
        );
        // OAuth records carry no bare API key; adapters consume them directly.
        assert!(!resolved.contains_key("chatgpt"));
    }

    #[test]
    fn new_credentials_use_haarness_style_typed_records() {
        assert_eq!(
            serde_json::to_value(Credential::api("secret".into())).unwrap(),
            serde_json::json!({"type": "api", "key": "secret"})
        );
    }

    #[test]
    fn permission_config_compiles_to_ordered_rules() {
        use haarness_core::PermissionDecision;
        let map: BTreeMap<String, toml::Value> = toml::from_str(
            r#"
            edit = "allow"
            "*" = "ask"
            [bash]
            "git status*" = "allow"
            "git push*" = "deny"
        "#,
        )
        .unwrap();
        let rules = super::compile_permissions(&map).unwrap();
        let patterns: Vec<_> = rules.iter().map(|rule| rule.pattern.as_str()).collect();
        // String entries compile first, pattern tables after — so specific
        // patterns win under last-match-wins.
        assert_eq!(
            patterns,
            vec!["*", "edit:*", "bash:git push*", "bash:git status*"]
        );
        assert_eq!(rules[0].decision, PermissionDecision::Ask);
        assert_eq!(rules[2].decision, PermissionDecision::Deny);

        let bad: BTreeMap<String, toml::Value> = toml::from_str(r#"edit = "maybe""#).unwrap();
        assert!(super::compile_permissions(&bad).is_err());
    }

    #[test]
    fn config_merges_global_then_trusted_project() {
        let root = std::env::temp_dir().join(format!("haarness-config-{}", uuid::Uuid::now_v7()));
        let config_dir = root.join("config");
        let workspace = root.join("ws");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(workspace.join(".haarness")).unwrap();
        std::fs::write(
            config_dir.join("config.toml"),
            "[defaults]\nprovider = \"openai\"\nmodel = \"gpt-5.4\"\nultracode = false\n[permissions]\nedit = \"ask\"\n[tool_availability]\nwebfetch = false\nsession_manage = false\n[lazy_tools]\nmode = \"auto\"\nmin_saved_tokens = 3000\npin = [\"mcp__github__*\"]\n[lsp]\nenabled = true\nbuiltins = true\n[formatter]\nenabled = false\n[experimental.goal_harness]\ntoken_budget = 40000\nmax_turns = 20\n",
        )
        .unwrap();
        std::fs::write(
            workspace.join(".haarness/config.toml"),
            "[defaults]\nmodel = \"gpt-5.4-mini\"\nultracode = true\n[permissions.bash]\n\"ls*\" = \"allow\"\n[tools]\nmax_rounds = 8\n[tool_availability]\nwebfetch = true\nmemory_save = false\n[lazy_tools]\nmode = \"always\"\nmin_saved_tokens = 1000\npin = [\"session_*\"]\n[lsp]\nenabled = false\n[formatter]\nenabled = true\nbuiltins = false\n[experimental.goal_harness]\nmax_turns = 6\n",
        )
        .unwrap();
        let paths = Paths {
            root: Some(root.clone()),
            data: root.join("data"),
            cache: root.join("cache"),
            database: root.join("data/haarness.db"),
            auth: config_dir.join("auth.json"),
            runtime: root.join("cache/runtime.json"),
            server_lock: root.join("cache/server.lock"),
            skills: root.join("data/skills"),
            config: config_dir.join("config.toml"),
            workspace_id: None,
        };

        let trusted = super::load_config(&paths, &workspace, true).unwrap();
        let defaults = trusted.defaults.clone().unwrap();
        assert_eq!(defaults.provider.as_deref(), Some("openai"));
        assert_eq!(
            defaults.model.as_deref(),
            Some("gpt-5.4-mini"),
            "project overrides global per key"
        );
        let runtime_config =
            super::runtime_config(&trusted, Vec::new(), Vec::new(), false).unwrap();
        assert_eq!(runtime_config.max_rounds, Some(8));
        assert_eq!(runtime_config.goal_token_budget, Some(40_000));
        assert_eq!(runtime_config.goal_max_turns, Some(6));
        assert!(runtime_config.default_ultracode);
        assert_eq!(runtime_config.base_rules.len(), 2);
        assert_eq!(
            runtime_config.tool_availability.get("webfetch"),
            Some(&true)
        );
        assert_eq!(
            runtime_config.tool_availability.get("session_manage"),
            Some(&false)
        );
        assert_eq!(
            runtime_config.tool_availability.get("memory_save"),
            Some(&false)
        );
        assert_eq!(
            runtime_config.lazy_tools.mode,
            haarness_protocol::LazyToolMode::Always
        );
        assert_eq!(runtime_config.lazy_tools.min_saved_tokens, 1_000);
        assert_eq!(runtime_config.lazy_tools.pin, ["session_*"]);
        assert!(!trusted.lsp.as_ref().unwrap().enabled);
        assert!(trusted.formatter.as_ref().unwrap().enabled);
        assert!(!trusted.formatter.as_ref().unwrap().builtins);

        let untrusted = super::load_config(&paths, &workspace, false).unwrap();
        assert_eq!(
            untrusted
                .defaults
                .as_ref()
                .and_then(|defaults| defaults.model.as_deref()),
            Some("gpt-5.4"),
            "untrusted project config must be ignored"
        );
        assert!(untrusted.tools.is_none());
        let untrusted_runtime =
            super::runtime_config(&untrusted, Vec::new(), Vec::new(), false).unwrap();
        assert_eq!(untrusted_runtime.goal_token_budget, Some(40_000));
        assert_eq!(untrusted_runtime.goal_max_turns, Some(20));
        assert_eq!(
            untrusted_runtime.tool_availability.get("webfetch"),
            Some(&false)
        );
        assert_eq!(
            untrusted_runtime.lazy_tools.mode,
            haarness_protocol::LazyToolMode::Auto
        );
        assert_eq!(untrusted_runtime.lazy_tools.min_saved_tokens, 3_000);
        assert_eq!(untrusted_runtime.lazy_tools.pin, ["mcp__github__*"]);
        assert!(untrusted.lsp.as_ref().unwrap().enabled);
        assert!(!untrusted.formatter.as_ref().unwrap().enabled);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn language_tooling_toml_supports_custom_and_per_entry_disable_controls() {
        let config: super::FileConfig = toml::from_str(
            r#"
            [lsp]
            enabled = true
            builtins = true

            [lsp.servers.rust]
            enabled = false

            [lsp.servers.acme]
            command = ["acme-lsp", "--stdio"]
            extensions = [".acme"]

            [lsp.servers.acme.language_ids]
            acme = "acme-language"

            [formatter]
            enabled = true
            builtins = true

            [formatter.formatters.prettier]
            enabled = false

            [formatter.formatters.acme]
            command = ["acme-format", "$FILE"]
            extensions = [".acme"]
            "#,
        )
        .unwrap();
        let lsp = config.lsp.unwrap();
        assert!(!lsp.servers["rust"].enabled);
        assert_eq!(lsp.servers["acme"].command[0], "acme-lsp");
        assert_eq!(lsp.servers["acme"].language_ids["acme"], "acme-language");
        let formatter = config.formatter.unwrap();
        assert!(formatter.enabled);
        assert!(!formatter.formatters["prettier"].enabled);
        assert_eq!(formatter.formatters["acme"].command[0], "acme-format");
    }

    #[test]
    fn goal_harness_toml_rejects_zero_limits() {
        let config: super::FileConfig =
            toml::from_str("[experimental.goal_harness]\ntoken_budget = 0\nmax_turns = 1\n")
                .unwrap();
        assert!(super::runtime_config(&config, Vec::new(), Vec::new(), false).is_err());
    }

    #[test]
    fn lazy_tool_toml_rejects_zero_threshold() {
        let config: super::FileConfig =
            toml::from_str("[lazy_tools]\nmin_saved_tokens = 0\n").unwrap();
        assert!(super::runtime_config(&config, Vec::new(), Vec::new(), false).is_err());
    }

    #[test]
    fn compaction_toml_validates_threshold_and_retained_turns() {
        for source in [
            "[compaction]\nthreshold = 0\n",
            "[compaction]\nthreshold = 1.01\n",
            "[compaction]\ntail_turns = 0\n",
            "[compaction]\npreserve_recent_tokens = 0\n",
        ] {
            let config: super::FileConfig = toml::from_str(source).unwrap();
            assert!(
                super::runtime_config(&config, Vec::new(), Vec::new(), false).is_err(),
                "accepted unsafe compaction config: {source}"
            );
        }
        let config: super::FileConfig = toml::from_str(
            "[compaction]\nenabled = false\nthreshold = 0.9\nreserve_output = 4096\ntail_turns = 3\npreserve_recent_tokens = 12000\n",
        )
        .unwrap();
        let runtime = super::runtime_config(&config, Vec::new(), Vec::new(), false).unwrap();
        assert!(!runtime.compaction.enabled);
        assert!((runtime.compaction.threshold - 0.9).abs() < f64::EPSILON);
        assert_eq!(runtime.compaction.reserve_output, Some(4096));
        assert_eq!(runtime.compaction.tail_turns, 3);
        assert_eq!(runtime.compaction.preserve_recent_tokens, 12_000);
    }

    #[test]
    fn json_goal_harness_defaults_feed_runtime_config() {
        let mut config = super::FileConfig::default();
        let layers = super::jsonconfig::JsonLayers {
            goal_token_budget: Some(24_000),
            goal_max_turns: Some(9),
            ..Default::default()
        };
        super::apply_json_goal_config(&mut config, &layers);
        let runtime = super::runtime_config(&config, Vec::new(), Vec::new(), false).unwrap();
        assert_eq!(runtime.goal_token_budget, Some(24_000));
        assert_eq!(runtime.goal_max_turns, Some(9));
    }

    #[test]
    fn provider_sections_compile_to_instances_and_suppressions() {
        let config: super::FileConfig = toml::from_str(
            r#"
            [providers.groq]
            models = ["llama-3.3-70b"]

            [providers.together]
            models = ["zai-org/GLM-5.1"]

            [providers.mylocal]
            kind = "openai-compatible"
            base_url = "http://127.0.0.1:9999/v1"
            name = "My Local"
            requires_auth = false
            images = true
            tools = false
            context_window = 131072

            [providers.ollama]
            enabled = false

            [providers.broken]
            # no base_url and not a preset

            [providers.claude-gateway]
            protocol = "anthropic-messages"
            base_url = "https://models.example.test/anthropic/v1"
            models = ["claude-compatible-model"]
            headers = { X-Tenant = "commercial" }

            [providers.gemini-gateway]
            protocol = "gemini-generate-content"
            base_url = "https://models.example.test/gemini/v1beta"
            models = ["gemini-compatible-model"]
            video = true

            [providers.azure-gateway]
            protocol = "azure-openai"
            base_url = "https://example.openai.azure.com/openai/v1"
            models = ["deployment-name"]

            [providers.vertex-gateway]
            protocol = "google-vertex"
            base_url = "https://us-central1-aiplatform.googleapis.com/v1/projects/test/locations/us-central1/publishers/google"
            models = ["gemini-test"]

            [providers.copilot-gateway]
            protocol = "github-copilot"
            base_url = "https://api.githubcopilot.com"
            models = ["gpt-test"]

            [providers.bedrock-gateway]
            protocol = "amazon-bedrock"
            base_url = "https://bedrock-runtime.us-east-1.amazonaws.com"
            models = ["amazon.nova-test-v1:0"]

            [providers.badwire]
            protocol = "vendor-private-wire"
            base_url = "https://example.invalid"

            [providers.disconnected]
            base_url = "https://example.invalid/v1"

            [providers.autodiscover]
            base_url = "https://models.example.test/v1"
            server_output_limit = true

            [providers.badvideo]
            base_url = "https://example.invalid/v1"
            video = true
        "#,
        )
        .unwrap();
        let credentials =
            std::collections::HashMap::from([("autodiscover".to_owned(), "stored-key".to_owned())]);
        let (instances, disabled) = super::provider_instances(&config, &credentials);

        assert!(disabled.contains("ollama"));
        let groq = instances
            .iter()
            .find(|instance| instance.id() == "groq")
            .unwrap();
        let haarness_providers::ProviderInstance::OpenAiChatCompletions(groq) = groq else {
            panic!("Groq must use Chat Completions")
        };
        assert_eq!(groq.base_url, "https://api.groq.com/openai/v1");
        assert_eq!(groq.name, "Groq");
        assert_eq!(groq.models, vec!["llama-3.3-70b".to_owned()]);
        assert!(!groq.discover_models, "explicit model list is an allowlist");
        assert!(
            !groq.server_output_limit,
            "ordinary providers retain client defaults"
        );
        let haarness_providers::ProviderInstance::OpenAiChatCompletions(managed) = instances
            .iter()
            .find(|instance| instance.id() == "autodiscover")
            .unwrap()
        else {
            panic!("managed gateway must use Chat Completions")
        };
        assert!(managed.server_output_limit);
        assert!(managed.discover_models);
        assert!(
            groq.api_key.is_none(),
            "stored auth stays live in CredentialStore"
        );
        let together = instances
            .iter()
            .find(|instance| instance.id() == "together")
            .unwrap();
        let haarness_providers::ProviderInstance::OpenAiChatCompletions(together) = together else {
            panic!("Together must use Chat Completions")
        };
        assert_eq!(together.name, "Together AI");
        assert!(together.api_key.is_none());
        let local = instances
            .iter()
            .find(|instance| instance.id() == "mylocal")
            .unwrap();
        let haarness_providers::ProviderInstance::OpenAiChatCompletions(local) = local else {
            panic!("local endpoint must use Chat Completions")
        };
        assert_eq!(local.name, "My Local");
        assert!(local.api_key.is_none());
        assert!(local.assume_authenticated);
        assert!(local.discover_models);
        assert_eq!(local.model_defaults.images, Some(true));
        assert_eq!(local.model_defaults.tools, Some(false));
        assert_eq!(local.model_defaults.context_window, Some(131_072));
        let disconnected = instances
            .iter()
            .find(|instance| instance.id() == "disconnected")
            .unwrap();
        let haarness_providers::ProviderInstance::OpenAiChatCompletions(disconnected) =
            disconnected
        else {
            panic!("disconnected endpoint must use Chat Completions")
        };
        assert!(disconnected.api_key.is_none());
        assert!(
            !disconnected.discover_models,
            "keyless authenticated remotes must not make startup requests"
        );
        let autodiscover = instances
            .iter()
            .find(|instance| instance.id() == "autodiscover")
            .unwrap();
        let haarness_providers::ProviderInstance::OpenAiChatCompletions(autodiscover) =
            autodiscover
        else {
            panic!("autodiscovery endpoint must use Chat Completions")
        };
        assert!(autodiscover.api_key.is_none(), "stored auth stays live");
        assert!(
            autodiscover.discover_models,
            "stored auth enables startup discovery without freezing the credential"
        );
        assert!(
            !instances.iter().any(|instance| instance.id() == "broken"),
            "sections without a resolvable base_url are skipped"
        );
        assert!(!instances.iter().any(|instance| instance.id() == "badwire"));
        assert!(!instances.iter().any(|instance| instance.id() == "badvideo"));

        let anthropic = instances
            .iter()
            .find(|instance| instance.id() == "claude-gateway")
            .unwrap();
        let haarness_providers::ProviderInstance::AnthropicMessages(anthropic) = anthropic else {
            panic!("custom Anthropic transport was not retained")
        };
        assert_eq!(anthropic.models, ["claude-compatible-model"]);
        assert!(!anthropic.headers.is_empty());

        let gemini = instances
            .iter()
            .find(|instance| instance.id() == "gemini-gateway")
            .unwrap();
        let haarness_providers::ProviderInstance::GeminiGenerateContent(gemini) = gemini else {
            panic!("custom Gemini transport was not retained")
        };
        assert_eq!(gemini.model_defaults.video, Some(true));

        assert!(matches!(
            instances
                .iter()
                .find(|instance| instance.id() == "azure-gateway"),
            Some(haarness_providers::ProviderInstance::AzureOpenAi(_))
        ));
        assert!(matches!(
            instances
                .iter()
                .find(|instance| instance.id() == "vertex-gateway"),
            Some(haarness_providers::ProviderInstance::GoogleVertex(_))
        ));
        assert!(matches!(
            instances
                .iter()
                .find(|instance| instance.id() == "copilot-gateway"),
            Some(haarness_providers::ProviderInstance::GithubCopilot(_))
        ));
        assert!(matches!(
            instances
                .iter()
                .find(|instance| instance.id() == "bedrock-gateway"),
            Some(haarness_providers::ProviderInstance::AmazonBedrock(_))
        ));
    }

    #[test]
    fn unknown_credential_records_survive_round_trips() {
        let root = std::env::temp_dir().join(format!("haarness-auth-{}", uuid::Uuid::now_v7()));
        let config_dir = root.join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        let paths = Paths {
            root: Some(root.clone()),
            data: root.join("data"),
            cache: root.join("cache"),
            database: root.join("data/haarness.db"),
            auth: config_dir.join("auth.json"),
            runtime: root.join("cache/runtime.json"),
            server_lock: root.join("cache/server.lock"),
            skills: root.join("data/skills"),
            config: config_dir.join("config.toml"),
            workspace_id: None,
        };
        std::fs::write(
            &paths.auth,
            r#"{"future": {"type": "quantum", "blob": "opaque"}, "openai": {"type": "api", "key": "sk"}}"#,
        )
        .unwrap();

        let mut auth = super::read_auth(&paths).unwrap();
        auth.insert(
            "anthropic".into(),
            serde_json::to_value(Credential::api("k2".into())).unwrap(),
        );
        super::write_auth(&paths, &auth).unwrap();

        let reread = super::read_auth(&paths).unwrap();
        assert_eq!(
            reread.get("future"),
            Some(&serde_json::json!({"type": "quantum", "blob": "opaque"})),
            "unknown record types must survive a read-modify-write cycle"
        );
        assert!(reread.contains_key("anthropic"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_database_migration_is_consistent_and_never_overwrites() {
        let root =
            std::env::temp_dir().join(format!("haarness-db-migrate-{}", uuid::Uuid::now_v7()));
        let paths = Paths::new(Some(&root)).unwrap();
        let legacy = paths.data.join("haarness.db");
        {
            let connection = rusqlite::Connection::open(&legacy).unwrap();
            connection
                .pragma_update(None, "journal_mode", "WAL")
                .unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE proof(value TEXT); INSERT INTO proof VALUES('preserved');",
                )
                .unwrap();
        }
        let destination = root.join("data/workspaces/test/haarness.db");
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        migrate_legacy_workspace_database(&paths, &destination).unwrap();
        assert!(!legacy.exists());
        let value = rusqlite::Connection::open(&destination)
            .unwrap()
            .query_row("SELECT value FROM proof", [], |row| row.get::<_, String>(0))
            .unwrap();
        assert_eq!(value, "preserved");

        let second_legacy = rusqlite::Connection::open(&legacy).unwrap();
        second_legacy
            .execute_batch("CREATE TABLE retry(value TEXT); INSERT INTO retry VALUES('source');")
            .unwrap();
        drop(second_legacy);
        let error = migrate_legacy_workspace_database(&paths, &destination).unwrap_err();
        assert!(error.to_string().contains("refusing to overwrite"));
        assert!(legacy.exists(), "the retry source remains authoritative");
        let original = rusqlite::Connection::open(&destination)
            .unwrap()
            .query_row("SELECT value FROM proof", [], |row| row.get::<_, String>(0))
            .unwrap();
        assert_eq!(original, "preserved");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn execution_settings_merge_per_field_and_validate() {
        let mut base: FileConfig =
            toml::from_str("[execution]\nshell_timeout_ms=0\nsubagent_concurrency=6\n").unwrap();
        let project: FileConfig = toml::from_str("[execution]\nsubagent_depth=3\n").unwrap();
        super::merge_file_config(&mut base, project);
        let config = super::runtime_config(&base, Vec::new(), Vec::new(), false).unwrap();
        assert_eq!(config.execution.shell_timeout_ms, 0);
        assert_eq!(config.execution.subagent_concurrency, 6);
        assert_eq!(config.execution.subagent_depth, 3);
    }
}
