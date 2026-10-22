//! Plain-data runtime configuration. Populated by the composition root from
//! configuration files; the core never parses files itself.

use crate::permissions::PermissionRule;
use std::collections::HashMap;

/// A user-defined agent from markdown/config: prompt + presentation.
#[derive(Debug, Clone, Default)]
pub struct CustomAgent {
    pub id: String,
    pub description: String,
    /// System prompt body; appended to the base agent scaffold.
    pub prompt: String,
    /// primary | subagent | all.
    pub mode: Option<String>,
    /// Pinned `provider/model` override.
    pub model: Option<String>,
    /// Per-agent tool policy. `false` cannot be reopened by the caller.
    pub tools: HashMap<String, bool>,
    /// Ordered permission rules applied after user configuration.
    pub permissions: Vec<PermissionRule>,
    /// Hidden from agent pickers and `@` autocomplete (`hidden`).
    pub hidden: bool,
    /// Presentation colour: a `#rrggbb` value or a theme name
    /// (`primary`, `secondary`, `accent`, `success`, `warning`, `error`,
    /// `info`). Carried for clients; the runtime does not interpret it.
    pub color: Option<String>,
    /// Maximum provider/tool rounds before the runtime asks for a final
    /// tool-free reply (`steps`). Overrides `tools.max_rounds`.
    pub steps: Option<u32>,
    /// Sampling temperature. Parsed and stored, but not yet applied: the
    /// provider request contract has no per-agent sampling fields.
    pub temperature: Option<f32>,
    /// Nucleus sampling. Parsed and stored, but not yet applied (see
    /// `temperature`).
    pub top_p: Option<f32>,
    /// Default model variant name. Parsed and stored, but not yet applied.
    pub variant: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputStyle {
    pub id: String,
    pub instructions: String,
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeConfig {
    pub execution: crate::ExecutionSettings,
    /// Canonical directories containing installed skill resources or owned scratch files.
    pub read_roots: Vec<std::path::PathBuf>,
    /// Ordered permission rules appended after the agent profile's defaults
    /// (last match wins).
    pub base_rules: Vec<PermissionRule>,
    /// "Always for this workspace" answers. Like session approvals they settle
    /// what the rules leave as a question and never reopen a configured deny.
    pub granted_rules: Vec<PermissionRule>,
    pub default_agent: Option<String>,
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    /// `provider/model` used for background work (titles, summaries).
    pub small_model: Option<String>,
    pub default_reasoning_effort: Option<String>,
    /// New root threads opt into standing Ultracode orchestration.
    pub default_ultracode: bool,
    /// Optional response-format guidance loaded by the composition root.
    pub output_style: Option<OutputStyle>,
    /// The built-in prompts, changed by the prompt files the composition root
    /// read (none: every prompt is its built-in text).
    pub prompts: crate::Prompts,
    /// Optional provider/tool round guard. `None` is unbounded;
    /// a configured value asks for a final tool-free reply
    /// instead of failing the turn.
    pub max_rounds: Option<u32>,
    /// Workspace tool availability baseline. Effective `false` removes the
    /// schema and rejects execution; a turn cannot reopen it. A durable
    /// operator override may intentionally change the workspace baseline.
    pub tool_availability: HashMap<String, bool>,
    /// Provider-context scaling for optional tool schemas.
    pub lazy_tools: crate::LazyToolSettings,
    /// Default token budget for a newly-set goal when the action omits one.
    pub goal_token_budget: Option<u64>,
    /// Autonomous continuations per goal; defaults to 20 when absent.
    pub goal_max_turns: Option<u32>,
    pub compaction: CompactionSettings,
    /// `serve --yolo`: auto-approve every would-be prompt in every chat on
    /// this server, recording the "always" rules it would have persisted.
    /// Clients' `--yolo` never sets this; their turns carry `auto_approve`.
    pub auto_approve: bool,
    /// User-defined slash commands (markdown files + config entries).
    pub commands: Vec<haarness_protocol::CommandInfo>,
    /// User-defined agents (markdown files + config entries).
    pub custom_agents: Vec<CustomAgent>,
    /// Disabled profiles stay installed but cannot admit new turns.
    pub disabled_agents: Vec<String>,
}

impl RuntimeConfig {
    /// Baseline exceptions are applied before explicit user/agent rules.
    pub fn permission_defaults(&self, agent: &str, tool: &str) -> crate::PermissionEngine {
        let mut engine = if agent == "plan" {
            crate::PermissionEngine::plan_defaults()
        } else {
            crate::PermissionEngine::build_defaults()
        };
        if agent == "plan" {
            for name in &self.execution.plan_read_tools {
                engine.add_ceiling(crate::PermissionRule {
                    pattern: format!("{name}:*"),
                    decision: crate::PermissionDecision::Allow,
                });
            }
        }
        if matches!(tool, "read" | "list" | "glob" | "grep") {
            for root in &self.read_roots {
                // Permission patterns use wildcards; ambiguous literal directory
                // names must retain the ordinary prompt instead of widening access.
                if root.to_string_lossy().contains(['*', '?']) {
                    continue;
                }
                engine.add_rule(crate::PermissionRule {
                    pattern: format!("external_directory:{}/*", root.display()),
                    decision: crate::PermissionDecision::Allow,
                });
            }
        }
        engine
    }
    /// Resolve one configured agent by its exact id.
    #[must_use]
    pub fn custom_agent(&self, agent: &str) -> Option<&CustomAgent> {
        self.custom_agents
            .iter()
            .find(|candidate| candidate.id == agent)
    }

    /// System prompt for `agent`: a custom agent's body rides on the base
    /// scaffold; built-ins keep their fixed prompts.
    #[must_use]
    pub fn system_prompt_for(&self, agent: &str) -> String {
        if let Some(custom) = self.custom_agent(agent) {
            return format!(
                "You are Haarness, a local agentic coding harness. This permission system is not an OS sandbox.

{}",
                custom.prompt.trim()
            );
        }
        crate::agents::system_prompt_for(agent)
    }
    #[must_use]
    pub fn command(&self, name: &str) -> Option<&haarness_protocol::CommandInfo> {
        self.commands
            .iter()
            .find(|command| command.name.eq_ignore_ascii_case(name))
    }
}

#[derive(Debug, Clone)]
pub struct CompactionSettings {
    pub enabled: bool,
    /// Fraction of the model context window that may trigger early
    /// auto-compaction. Model output headroom always remains authoritative.
    pub threshold: f64,
    /// Optional additional output reserve. The effective reserve is never
    /// smaller than the selected model's advertised maximum output.
    pub reserve_output: Option<u64>,
    /// Number of recent user turns retained verbatim behind each summary.
    pub tail_turns: usize,
    /// Upper bound for the retained verbatim tail. The runtime further caps
    /// this at half of the selected model's safe input budget.
    pub preserve_recent_tokens: u64,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold: 0.85,
            reserve_output: None,
            tail_turns: 2,
            preserve_recent_tokens: 24_000,
        }
    }
}
