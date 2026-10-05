//! Provider, storage, and transport independent agent runtime.

mod agents;
mod config;
pub mod definitions;
mod events;
mod execution;
mod goal;
pub mod instructions;
pub mod patch;
mod permissions;
mod ports;
mod runtime;
mod side_question;
mod tool_catalog;
mod tool_validation;
mod workflows;

pub use agents::{
    SystemPromptContext, ULTRACODE_SYSTEM_SECTION, builtin_profiles, system_prompt_for,
    system_prompt_with_context,
};
pub use config::{CompactionSettings, CustomAgent, OutputStyle, RuntimeConfig};
pub use events::{EventBus, EventSubscription};
pub use execution::ExecutionSettings;
pub use goal::{MAX_GOAL_CHARS, apply_goal_action, goal_line, legacy_goal_state, new_goal_state};
pub use permissions::bash::is_obviously_read_only as bash_is_obviously_read_only;
pub use permissions::{
    PermissionDecision, PermissionEngine, PermissionRule, resolve_permission_path,
};
pub use ports::*;
pub use runtime::{
    AgentRuntime, ContextUsage, DirectToolResult, EmptyToolRegistry, StaticProviderRegistry,
};
pub use tool_catalog::{LOAD_TOOL_NAME, LazyToolSettings, load_tool_definition, select_tools};
pub use workflows::{WorkflowLogDraft, WorkflowRepository};
