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
pub mod links;
pub mod patch;
mod permissions;
mod ports;
mod prefix_warmup;
pub mod process_groups;
pub mod prompts;
pub mod redact;
mod runtime;
pub mod shell_text;
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
pub use events::{
    EncodedEvent, EventBus, EventSubscription, PENDING_EVENTS_LIMIT, PendingEvents, join_delta,
};
pub use execution::ExecutionSettings;
pub use goal::{MAX_GOAL_CHARS, apply_goal_action, goal_line, legacy_goal_state, new_goal_state};
pub use permissions::bash::is_obviously_read_only as bash_is_obviously_read_only;
pub use permissions::{
    CONFIG_EDIT_RULE, EXPLICIT_GRANT_PREFIX, PermissionDecision, PermissionEngine, PermissionRule,
    explicit_decision, explicit_granted, forget_rule_links, global_config_dir_from,
    is_base_config_path, is_config_path, is_global_config_path, is_secret_path,
    resolve_permission_entry, resolve_permission_path, set_base_config_dir, set_global_config_dir,
};
pub use ports::*;
pub use prompts::{PromptFile, PromptMode, PromptName, Prompts};
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

/// The variable that names the chat an agent's shell command runs for. The
/// shell tool sets it for each command the agent runs (see
/// [`ExecutionSettings::person_shell`]); no other process the server starts
/// inherits it (see [`private_env_names`]).
pub const SESSION_ID_ENV: &str = "HAARNESS_SESSION_ID";

/// Variables a process the server starts for the agent, a terminal, a hook, an
/// MCP server or an extension must not inherit: the server's own credentials,
/// the OpenTelemetry settings (`OTEL_EXPORTER_OTLP_HEADERS` usually holds a
/// collector key, and instrumented programs would export into the user's
/// collector), and [`SESSION_ID_ENV`], which a server started by an agent's
/// command inherits from that command and which names none of its own chats.
/// Configured environments (`shell.env` hooks, MCP `env`) can still set them
/// explicitly.
#[must_use]
pub fn private_env_names() -> Vec<std::ffi::OsString> {
    std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| {
            name.to_str()
                .is_some_and(|name| is_private_env_name(name, cfg!(windows)))
        })
        .collect()
}

/// Whether `name` is one of [`private_env_names`]. Windows finds a variable
/// whatever the case of its name, so the server reads `haarness_server_token`
/// as its token there, and that spelling is private too (`any_case`).
fn is_private_env_name(name: &str, any_case: bool) -> bool {
    let same = |left: &str, right: &str| {
        if any_case {
            left.eq_ignore_ascii_case(right)
        } else {
            left == right
        }
    };
    name.get(..5).is_some_and(|start| same(start, "OTEL_"))
        || [
            "HAARNESS_SERVER_TOKEN",
            "HAARNESS_SERVER_PASSWORD",
            "HAARNESS_SERVER_USERNAME",
            SESSION_ID_ENV,
        ]
        .iter()
        .any(|private| same(name, private))
}

#[cfg(test)]
mod private_env_tests {
    use super::is_private_env_name;

    #[test]
    fn the_server_s_credentials_are_private_in_any_case_where_names_ignore_case() {
        for name in [
            "HAARNESS_SERVER_TOKEN",
            "HAARNESS_SERVER_PASSWORD",
            "HAARNESS_SERVER_USERNAME",
            "OTEL_EXPORTER_OTLP_HEADERS",
            "HAARNESS_SESSION_ID",
        ] {
            assert!(is_private_env_name(name, false), "{name}");
            assert!(is_private_env_name(name, true), "{name}");
        }
        for name in [
            "haarness_server_token",
            "Haarness_Server_Password",
            "otel_exporter_otlp_headers",
            "haarness_session_id",
        ] {
            assert!(is_private_env_name(name, true), "{name}");
            assert!(!is_private_env_name(name, false), "{name}");
        }
        for name in [
            "PATH",
            "OTEL",
            "HAARNESS_SERVER",
            "HAARNESS_SERVER_TOKENS",
            "HAARNESS_SESSION",
            "AI_AGENT",
            "AGENT",
            "HAARNESS",
            "é",
        ] {
            assert!(!is_private_env_name(name, true), "{name}");
        }
    }
}
