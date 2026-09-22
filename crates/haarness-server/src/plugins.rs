use haarness_protocol::{PluginActionRequest, PluginInfo};

/// Server-facing plugin administration port. The CLI owns filesystem and
/// marketplace details; the REST/TUI layer only sees this narrow contract.
pub trait PluginRegistry: Send + Sync {
    fn list(&self) -> anyhow::Result<Vec<PluginInfo>>;
    fn apply(&self, request: &PluginActionRequest) -> anyhow::Result<Vec<PluginInfo>>;
}
