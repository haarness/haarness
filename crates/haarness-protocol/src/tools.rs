//! Tool-catalog and context-scaling wire contracts.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Controls whether non-essential tool schemas are loaded on demand.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LazyToolMode {
    /// Always advertise every enabled tool schema.
    Off,
    /// Defer non-essential schemas when doing so saves meaningful context.
    #[default]
    Auto,
    /// Always defer non-essential schemas behind `load_tool`.
    Always,
}

impl LazyToolMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Auto => "auto",
            Self::Always => "always",
        }
    }
}

impl std::str::FromStr for LazyToolMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" | "disabled" | "false" => Ok(Self::Off),
            "auto" => Ok(Self::Auto),
            "always" | "on" | "enabled" | "true" => Ok(Self::Always),
            _ => Err(format!(
                "lazy tool mode must be off, auto, or always; got {value}"
            )),
        }
    }
}

/// One provider-visible or deferred tool in the current runtime catalog.
// These are independent wire facts, not mutually exclusive states: for
// example, an essential tool can be disabled, and a loaded tool can cease to
// be advertised after a live availability update.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolCatalogEntry {
    pub id: String,
    pub description: String,
    /// Stable functional grouping such as `core`, `agents`, or `extensions`.
    pub group: String,
    pub group_title: String,
    /// `native` or the projected MCP server id.
    pub source: String,
    /// Essential tools stay resident when lazy loading is active.
    pub essential: bool,
    /// False means the tool is unavailable at both schema and execution time.
    pub enabled: bool,
    /// Whether this schema would be sent on the next matching provider round.
    pub advertised: bool,
    /// Whether this session explicitly promoted a deferred tool.
    pub loaded: bool,
    /// Deterministic provider-schema estimate; deliberately labeled as an estimate.
    pub estimated_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolCatalogResponse {
    pub tools: Vec<ToolCatalogEntry>,
    pub lazy_mode: LazyToolMode,
    pub lazy_active: bool,
    pub estimated_total_tokens: u64,
    pub estimated_advertised_tokens: u64,
    pub estimated_saved_tokens: u64,
}

/// Live, durable workspace-scoped tool policy update.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolSettingsUpdate {
    /// `Some` sets an override; omitted removes the persisted override.
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LazyToolSettingsUpdate {
    /// Sets a durable live override. Omit or send null to return to the
    /// configured/default mode, matching per-tool availability reset semantics.
    #[serde(default)]
    pub mode: Option<LazyToolMode>,
}

#[cfg(test)]
mod tests {
    use super::{LazyToolMode, LazyToolSettingsUpdate};

    #[test]
    fn lazy_mode_has_stable_wire_names_and_strict_parser() {
        assert_eq!(
            serde_json::to_string(&LazyToolMode::Always).unwrap(),
            "\"always\""
        );
        assert_eq!("off".parse(), Ok(LazyToolMode::Off));
        assert!("sometimes".parse::<LazyToolMode>().is_err());
    }

    #[test]
    fn omitted_lazy_mode_is_an_explicit_override_reset() {
        let update: LazyToolSettingsUpdate = serde_json::from_str("{}").unwrap();
        assert_eq!(update.mode, None);
        let update: LazyToolSettingsUpdate = serde_json::from_str(r#"{"mode":null}"#).unwrap();
        assert_eq!(update.mode, None);
    }
}
