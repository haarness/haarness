//! Provider, storage, and transport independent agent runtime.

pub mod agent_shell;
mod agents;
mod approval_mode;
pub mod attachments;
pub mod auto_review;
mod background;
mod background_commands;
pub mod chat_folders;
mod config;
pub mod definitions;
mod events;
mod execution;
mod goal;
mod image_budget;
pub mod images;
pub mod instructions;
pub mod links;
pub mod mcp_content;
pub mod patch;
pub mod paths;
mod permissions;
mod ports;
mod prefix_warmup;
pub mod process_groups;
pub mod process_tree;
pub mod prompts;
pub mod redact;
mod request_ceilings;
mod runtime;
pub mod shared_chats;
pub mod shell_text;
mod side_question;
mod stream_clock;
mod stream_limits;
mod tool_catalog;
mod tool_validation;
mod turn_context;
mod unanswered;
pub mod wire_names;
mod workflows;

pub use agents::{
    SystemPromptContext, ULTRACODE_SYSTEM_SECTION, builtin_profiles, system_prompt_for,
    system_prompt_with_context,
};
pub use auto_review::AutoReviewSettings;
pub use background::{BackgroundHandoff, BackgroundProbe};
pub use background_commands::{
    BACKGROUND_FILE_PREFIX, CommandOutput, CommandProcess, CommandRequest, CommandRun,
    OUTPUT_SAVED_LIMIT, OUTPUT_TAIL_KEPT, OutputPage, ProcessControl, ProcessEnd, ShellCall,
    StopSignal, spoken_duration, whole_end, whole_start,
};
pub use config::{CompactionSettings, CustomAgent, OutputStyle, RuntimeConfig};
pub use events::{
    EncodedEvent, EventBus, EventSubscription, PENDING_EVENTS_LIMIT, PendingEvents, join_delta,
};
pub use execution::ExecutionSettings;
pub use goal::{MAX_GOAL_CHARS, apply_goal_action, goal_line, legacy_goal_state, new_goal_state};
pub use permissions::bash::is_obviously_read_only as bash_is_obviously_read_only;
pub use permissions::{
    CONFIG_EDIT_RULE, CREDENTIAL_READ_RULE, EXPLICIT_GRANT_PREFIX, PermissionDecision,
    PermissionEngine, PermissionRule, expand_home, explicit_decision, explicit_granted,
    forget_rule_links, global_config_dir, global_config_dir_from, is_config_path,
    is_credential_store_path, is_global_config_path, is_secret_path, is_system_config_path,
    record_credential_files_below, record_credential_store, resolve_permission_entry,
    resolve_permission_path, set_global_config_dir, set_system_config_dir,
};
pub use ports::*;
pub use prompts::{PromptFile, PromptMode, PromptName, Prompts};
pub use runtime::{
    AgentRuntime, ContextUsage, DirectToolResult, EmptyToolRegistry, PROMPT_BLOCKED,
    ResourceMentionPolicy, SUBAGENT_TASK_DESCRIPTION, StaticProviderRegistry, is_stream_cut,
    prompt_blocked, refusal_reason, shorten_title, webfetch_address, webfetch_redirect_verdict,
};
pub use stream_limits::{
    DEFAULT_RESPONSE_TOTAL, DEFAULT_STREAM_IDLE, StreamLimits, response_timeout, stream_stall,
    stream_stall_silence,
};
pub use tool_catalog::{
    LOAD_TOOL_NAME, LazyToolSettings, SKILL_LIST_BUDGET, code_can_call, load_tool_definition,
    select_tools, skill_list, some_names,
};
pub use tool_validation::{shell_call_from_words, tool_call_arguments, with_field_aliases};
pub use turn_context::{
    AGENT_MESSAGE_PREFIX, SESSION_MESSAGE_PREFIX, is_session_message, is_turn_reminder,
};
pub use unanswered::{
    ToolCallPairs, UNFINISHED_CALL_RESULT, not_run, not_run_metadata, pair_tool_calls,
    unfinished_call_result,
};
pub use workflows::{WorkflowLogDraft, WorkflowRepository};

/// The `tracing` target for events that may be exported with OpenTelemetry
/// traces. Other log lines stay in the local log files: their fields can carry
/// provider error bodies, approval prompts, paths and client addresses.
pub const TRACE_EVENT_TARGET: &str = "haarness::trace";

/// How a provider error begins when the provider could not read a tool call
/// the model wrote and ended its reply (Ollama's parsers for gpt-oss, Qwen
/// and others do): the provider's own reason follows. The runtime asks the
/// model to write the call again, as it does when a provider refuses a call
/// that does not match its tool.
pub const UNREADABLE_TOOL_CALL: &str = "the provider couldn't read a tool call the model wrote: ";

/// Why a chat refuses a new turn while one is running: read by people, from
/// `haarness run --session` or an editor over ACP sending to a chat busy
/// elsewhere.
pub const CHAT_BUSY: &str = "This chat already has a turn running, started in another window or run. It takes a new message once that turn finishes or is stopped.";

/// Why the server refuses to let a chat's agents do more without asking when
/// the change comes from a program that runs for an agent (an agent's
/// command, a hook; see [`process_tree::request_from_agent`]).
pub const APPROVALS_RAISE_REFUSAL: &str = "Only you can let agents do more without asking. This change came from a program Haarness started, such as an agent's command or a hook, so the chat's approvals did not change. Switch them in the chat, or run haarness --approvals yourself.";

/// Why the server refuses a turn that approves its own requests from a
/// program that runs for an agent. A scheduled run's own runner may start one.
pub const AUTO_APPROVE_REFUSAL: &str = "Only you can start a turn that approves its own requests. This request came from a program Haarness started, such as an agent's command or a hook, so no turn started. Run haarness --yolo yourself.";

/// Why the server refuses an answer that allows what an agent asked for when
/// it comes from a program that runs for an agent. A no is never refused.
pub const PERMISSION_ANSWER_REFUSAL: &str = "Only you can allow what an agent asks for. This answer came from a program Haarness started, such as an agent's command or a hook, so the request is still waiting for you.";

/// Why the server refuses an answer to an agent's question from a program
/// that runs for an agent.
pub const QUESTION_ANSWER_REFUSAL: &str = "Only you can answer an agent's question. This answer came from a program Haarness started, such as an agent's command or a hook, so the question is still waiting for you.";

/// Why the server refuses to set up, change, switch on or start a schedule for
/// a program that runs for an agent: a schedule's runs approve their own
/// requests.
pub const SCHEDULE_REFUSAL: &str = "Only you can set up or start a schedule: its runs approve their own requests. This request came from a program Haarness started, such as an agent's command or a hook, so nothing changed.";

/// Why the server refuses a request that would let agents do more when the
/// program on this computer that sent it cannot be found. A client may try
/// again.
pub const UNKNOWN_SENDER: &str = "Haarness could not tell which program on this computer sent this request, so nothing changed. Try again.";

/// Why the server refuses its forwarding key to a program that runs for an
/// agent: with it, a request could say whom it was forwarded for.
pub const FORWARDING_REFUSAL: &str = "Only a program you run may forward requests to this server for others. This request came from a program Haarness started, such as an agent's command or a hook.";

/// Why the server refuses to act as a browser for a program that runs for an
/// agent: a browser host's traffic, and what it opens, count as a person's.
pub const BROWSER_HOST_REFUSAL: &str = "Only you can connect a browser to Haarness. This request came from a program Haarness started, such as an agent's command or a hook.";

/// Why a browser tunnel will not reach a Haarness server on this computer:
/// what arrives through it would come from the server itself.
pub const TUNNEL_REFUSAL: &str = "A browser here cannot open Haarness's own server.";

/// Why the server refuses to open or connect a terminal for a program that
/// runs for an agent: what runs in a terminal counts as a person's.
pub const TERMINAL_REFUSAL: &str = "Only you can open or use a terminal here. This request came from a program Haarness started, such as an agent's command or a hook.";

/// The variable that names the chat an agent's shell command runs for. The
/// shell tool sets it for each command the agent runs (see
/// [`ExecutionSettings::person_shell`]); the other processes the server
/// starts leave it out (see [`private_env_names`]).
pub const SESSION_ID_ENV: &str = "HAARNESS_SESSION_ID";

/// The `AI_AGENT` the shell tool gives a command the agent runs, unless an
/// outer agent already set its own.
pub const AI_AGENT_VALUE: &str = "haarness";

/// The variables of Haarness's own agent markers, as an agent's shell command
/// gets them, in an environment whose `AI_AGENT` is `ai_agent`: `AGENT`,
/// `HAARNESS`, [`SESSION_ID_ENV`], and `AI_AGENT` when it is
/// [`AI_AGENT_VALUE`]. A server started from such a command leaves them out:
/// it outlives that command and serves a person's own `!` commands and
/// terminals too, and its own agent commands are marked again. An outer
/// agent's `AI_AGENT` (any other value) is kept.
#[must_use]
pub fn own_agent_markers(ai_agent: Option<&std::ffi::OsStr>) -> Vec<&'static str> {
    let mut names = vec!["AGENT", "HAARNESS", SESSION_ID_ENV];
    if ai_agent.is_some_and(|value| value == AI_AGENT_VALUE) {
        names.push("AI_AGENT");
    }
    names
}

/// Variables a process the server starts for the agent, a terminal, a hook, an
/// MCP server, an extension, a language server, a formatter or Git must not
/// inherit: the server's own credentials, the OpenTelemetry settings
/// (`OTEL_EXPORTER_OTLP_HEADERS` usually holds a collector key, and
/// instrumented programs would export into the user's collector), and
/// [`SESSION_ID_ENV`], which a server started by an agent's command inherits
/// from that command and which names none of its own chats. Configured
/// environments (`shell.env` hooks, MCP `env`, a language server's
/// `environment`) can still set them explicitly.
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
    use super::{is_private_env_name, own_agent_markers};

    #[test]
    fn a_server_leaves_out_haarness_s_own_markers_and_keeps_an_outer_agent_s() {
        let own = ["AGENT", "HAARNESS", "HAARNESS_SESSION_ID"];
        assert_eq!(own_agent_markers(None), own);
        assert_eq!(own_agent_markers(Some("outer-agent".as_ref())), own);
        assert_eq!(own_agent_markers(Some("".as_ref())), own);
        assert_eq!(
            own_agent_markers(Some("haarness".as_ref())),
            ["AGENT", "HAARNESS", "HAARNESS_SESSION_ID", "AI_AGENT"]
        );
    }

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
