//! REST and SSE adapter with optional password authentication. Managed local
//! composition retains bearer authentication; every mode validates Host/Origin.

pub mod api;
pub mod auth;
pub mod browser;
pub mod browser_policy;
mod cloud_tools;
mod command;
mod data_folder;
pub mod files;
pub mod front;
pub mod hooks;
mod mcp_resources;
pub mod models;
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
pub use data_folder::{data_folder, use_data_folder};
pub use hooks::{ConfiguredHooks, HookEntry};
pub use plugins::PluginRegistry;
pub use skills::FilesystemSkillRegistry;
pub use tools::{ManagementRegistry, NativeToolRegistry};
pub use workflow::WorkflowRuntimeAdapter;

mod shell;
