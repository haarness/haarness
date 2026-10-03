//! REST and SSE adapter with optional password authentication. Managed local
//! composition retains bearer authentication; every mode validates Host/Origin.

pub mod api;
pub mod browser;
pub mod browser_policy;
pub mod auth;
mod command;
pub mod files;
pub mod preview;
pub mod hooks;
pub mod plugins;
mod profiles;
pub mod remote;
mod remote_scope;
pub mod sharing;
pub mod skills;
pub mod tools;
mod cloud_tools;
pub mod vcs;
pub mod workflow;

pub use api::{ServerHandle, ServerState, serve};
pub use hooks::{ConfiguredHooks, HookEntry};
pub use plugins::PluginRegistry;
pub use skills::FilesystemSkillRegistry;
pub use tools::{ManagementRegistry, NativeToolRegistry};
pub use workflow::WorkflowRuntimeAdapter;
mod shell;
