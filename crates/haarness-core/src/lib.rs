//! Provider, storage, and transport independent agent runtime.

mod agents;
mod approval_mode;
pub mod attachments;
mod background;
mod config;
pub mod definitions;
mod events;
mod execution;
mod goal;
pub mod images;
pub mod instructions;
pub mod patch;
mod permissions;
mod ports;
mod prefix_warmup;
pub mod redact;
mod runtime;
mod side_question;
mod stream_clock;
mod stream_limits;
mod tool_catalog;
mod tool_validation;
mod turn_context;
mod workflows;

pub use agents::{
    SystemPromptContext, ULTRACODE_SYSTEM_SECTION, builtin_profiles, system_prompt_for,
    system_prompt_with_context,
};
pub use background::{BackgroundHandoff, BackgroundProbe};
pub use config::{CompactionSettings, CustomAgent, OutputStyle, RuntimeConfig};
pub use events::{EventBus, EventSubscription, PENDING_EVENTS_LIMIT, PendingEvents, join_delta};
pub use execution::ExecutionSettings;
pub use goal::{MAX_GOAL_CHARS, apply_goal_action, goal_line, legacy_goal_state, new_goal_state};
pub use permissions::bash::is_obviously_read_only as bash_is_obviously_read_only;
pub use permissions::{
    CONFIG_EDIT_RULE, EXPLICIT_GRANT_PREFIX, PermissionDecision, PermissionEngine, PermissionRule,
    explicit_decision, explicit_granted, global_config_dir_from, is_config_path,
    is_global_config_path, is_secret_path, resolve_permission_path, set_global_config_dir,
};
pub use ports::*;
pub use runtime::{
    AgentRuntime, ContextUsage, DirectToolResult, EmptyToolRegistry, StaticProviderRegistry,
    refusal_reason, shorten_title,
};
pub use stream_limits::{
    DEFAULT_RESPONSE_TOTAL, DEFAULT_STREAM_IDLE, StreamLimits, response_timeout, stream_stall,
    stream_stall_silence,
};
pub use tool_catalog::{LOAD_TOOL_NAME, LazyToolSettings, load_tool_definition, select_tools};
pub use turn_context::is_turn_reminder;
pub use workflows::{WorkflowLogDraft, WorkflowRepository};

/// The `tracing` target for events that may be exported with OpenTelemetry
/// traces. Other log lines stay in the local log files: their fields can carry
/// provider error bodies, approval prompts, paths and client addresses.
pub const TRACE_EVENT_TARGET: &str = "haarness::trace";

/// Variables a process the server starts for the agent, a terminal, a hook, an
/// MCP server or an extension must not inherit: the server's own credentials,
/// and the OpenTelemetry settings (`OTEL_EXPORTER_OTLP_HEADERS` usually holds a
/// collector key, and instrumented programs would export into the user's
/// collector). Configured environments (`shell.env` hooks, MCP `env`) can still
/// set them explicitly.
#[must_use]
pub fn private_env_names() -> Vec<std::ffi::OsString> {
    std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| {
            name.to_str().is_some_and(|name| {
                name.starts_with("OTEL_")
                    || matches!(
                        name,
                        "HAARNESS_SERVER_TOKEN"
                            | "HAARNESS_SERVER_PASSWORD"
                            | "HAARNESS_SERVER_USERNAME"
                    )
            })
        })
        .collect()
}
