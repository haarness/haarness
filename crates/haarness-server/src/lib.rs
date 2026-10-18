//! REST and SSE adapter with optional password authentication. Managed local
//! composition retains bearer authentication; every mode validates Host/Origin.

pub mod api;
pub mod auth;
pub mod browser;
pub mod browser_policy;
mod cloud_tools;
mod command;
pub mod files;
pub mod hooks;
pub mod plugins;
pub mod preview;
mod profiles;
pub mod remote;
mod remote_scope;
pub mod sharing;
pub mod skills;
pub mod tools;
pub mod vcs;
pub mod workflow;

pub use api::{ServerHandle, ServerState, serve};
pub use hooks::{ConfiguredHooks, HookEntry};
pub use plugins::PluginRegistry;
pub use skills::FilesystemSkillRegistry;
pub use tools::{ManagementRegistry, NativeToolRegistry};
pub use workflow::WorkflowRuntimeAdapter;

/// Known credential shapes in free text replaced with `[redacted]`, as hook
/// log lines get them: for logs that leave the machine (`haarness logs
/// export`).
#[must_use]
pub fn redact_log_text(text: &str) -> String {
    hooks::redact(text)
}
mod shell;
