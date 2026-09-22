//! Provider-neutral tool catalog, availability policy, and lazy schema selection.

use std::{
    collections::{HashMap, HashSet},
    hash::BuildHasher,
};

use haarness_protocol::{LazyToolMode, ToolCatalogEntry, ToolCatalogResponse};
use serde_json::{Value, json};

use crate::ToolDefinition;

pub const LOAD_TOOL_NAME: &str = "load_tool";
pub const DEFAULT_LAZY_MIN_SAVED_TOKENS: u64 = 2_048;
pub const MAX_PROMOTED_TOOLS: usize = 64;
const MAX_LOAD_MATCHES: usize = 20;

#[derive(Debug, Clone)]
pub struct LazyToolSettings {
    pub mode: LazyToolMode,
    pub min_saved_tokens: u64,
    /// Exact ids or `*` wildcard patterns that remain resident.
    pub pin: Vec<String>,
}

impl Default for LazyToolSettings {
    fn default() -> Self {
        Self {
            mode: LazyToolMode::Auto,
            min_saved_tokens: DEFAULT_LAZY_MIN_SAVED_TOKENS,
            pin: Vec::new(),
        }
    }
}

/// A compact search tool replaces deferred schemas. Search results become
/// provider-visible on the following round and remain promoted for the thread.
#[must_use]
pub fn load_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: LOAD_TOOL_NAME.into(),
        description: "Find and load deferred tool schemas into this session. Use this when the required capability is not currently available. Search by exact tool id, wildcard (for example mcp__github__*), functional group, or words from a tool's name/description. Matching schemas are available on the next model round and stay loaded for this session.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "queries": {
                    "type": "array",
                    "description": "One to ten exact ids, wildcard patterns, groups, or search phrases.",
                    "items": {"type": "string", "minLength": 1},
                    "minItems": 1,
                    "maxItems": 10
                }
            },
            "required": ["queries"],
            "additionalProperties": false
        }),
    }
}

/// Estimate serialized provider-schema cost. Providers tokenize differently,
/// so the catalog reports a conservative deterministic estimate rather than
/// pretending to be an exact model tokenizer.
#[must_use]
pub fn estimated_tokens(tool: &ToolDefinition) -> u64 {
    let bytes = serde_json::to_vec(tool).map_or(0, |encoded| encoded.len());
    u64::try_from(bytes.div_ceil(4))
        .unwrap_or(u64::MAX)
        .saturating_add(8)
}

#[derive(Debug, Clone, Copy)]
struct Classification {
    group: &'static str,
    title: &'static str,
    essential: bool,
}

fn classify(name: &str) -> Classification {
    let group = if matches!(
        name,
        "task" | "task_abort" | "agent_switch" | "agent_manage"
    ) {
        ("agents", "Agents", matches!(name, "task"))
    } else if matches!(name, "todo" | "todowrite" | "todoread" | "question") {
        ("planning", "Planning", true)
    } else if name.starts_with("memory_") {
        ("memory", "Memory", false)
    } else if matches!(name, "get_goal" | "update_goal" | "propose_goal_criteria") {
        ("goals", "Goals", true)
    } else if name.starts_with("session_") {
        ("sessions", "Sessions", false)
    } else if matches!(name, "workflow" | "schedule") {
        ("automation", "Automation", false)
    } else if name == "webfetch" || name.starts_with("web_") {
        ("web", "Web", true)
    } else if matches!(name, "image_generate" | "video_generate") {
        ("media", "Media", false)
    } else if name.starts_with("ext__")
        || name.starts_with("mcp__")
        || matches!(
            name,
            "mcp_manage" | "plugin_manage" | "skill_manage" | "command_manage" | "skill"
        )
    {
        ("extensions", "Extensions", false)
    } else {
        // File, shell, patch, and any unknown native primitives remain
        // resident. A newly-added mutation tool must not silently disappear
        // behind lazy loading before it has been deliberately classified.
        ("core", "Core", true)
    };
    Classification {
        group: group.0,
        title: group.1,
        essential: group.2,
    }
}

#[must_use]
pub fn is_essential_tool(name: &str) -> bool {
    classify(name).essential
}

#[must_use]
pub fn tool_source(name: &str) -> String {
    if let Some(rest) = name.strip_prefix("ext__") {
        return format!("extension:{}", rest.split("__").next().unwrap_or(rest));
    }
    let Some(rest) = name.strip_prefix("mcp__") else {
        return "native".into();
    };
    let server = rest.split("__").next().unwrap_or(rest);
    format!("mcp:{server}")
}

fn wildcard_matches(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let mut remaining = value;
    let mut first = true;
    for part in pattern.split('*') {
        if part.is_empty() {
            first = false;
            continue;
        }
        let Some(index) = remaining.find(part) else {
            return false;
        };
        if first && !pattern.starts_with('*') && index != 0 {
            return false;
        }
        remaining = &remaining[index + part.len()..];
        first = false;
    }
    pattern.ends_with('*') || remaining.is_empty()
}

fn pinned(settings: &LazyToolSettings, name: &str) -> bool {
    settings
        .pin
        .iter()
        .any(|pattern| wildcard_matches(pattern, name))
}

fn effective_enabled<ConfiguredHasher, LiveHasher>(
    configured: &HashMap<String, bool, ConfiguredHasher>,
    live: &HashMap<String, bool, LiveHasher>,
    name: &str,
) -> bool
where
    ConfiguredHasher: BuildHasher,
    LiveHasher: BuildHasher,
{
    live.get(name)
        .or_else(|| configured.get(name))
        .copied()
        .unwrap_or(true)
}

/// Auto mode activates once deferring optional schemas saves the configured
/// minimum. For small-context models, 2% of the window is allowed to lower
/// that floor (never below 512 tokens). Missing context metadata leaves the
/// full catalog visible; operators can explicitly select `always`.
#[must_use]
pub fn lazy_active(
    mode: LazyToolMode,
    min_saved_tokens: u64,
    context_window: Option<u64>,
    deferred_tokens: u64,
    loader_tokens: u64,
) -> bool {
    match mode {
        LazyToolMode::Off => false,
        LazyToolMode::Always => deferred_tokens > 0,
        LazyToolMode::Auto => context_window.is_some_and(|window| {
            let threshold = min_saved_tokens.min((window / 50).max(512));
            deferred_tokens.saturating_sub(loader_tokens) >= threshold
        }),
    }
}

#[derive(Debug, Clone)]
pub struct ToolSelection {
    pub response: ToolCatalogResponse,
    pub advertised: Vec<ToolDefinition>,
}

/// Build both the public catalog and the exact provider-facing schema list
/// from one snapshot, preventing UI estimates from drifting from execution.
#[must_use]
pub fn select_tools<ConfiguredHasher, LiveHasher, PromotedHasher>(
    mut definitions: Vec<ToolDefinition>,
    configured: &HashMap<String, bool, ConfiguredHasher>,
    live: &HashMap<String, bool, LiveHasher>,
    settings: &LazyToolSettings,
    promoted: &HashSet<String, PromotedHasher>,
    context_window: Option<u64>,
) -> ToolSelection
where
    ConfiguredHasher: BuildHasher,
    LiveHasher: BuildHasher,
    PromotedHasher: BuildHasher,
{
    definitions.sort_by(|left, right| left.name.cmp(&right.name));
    definitions.dedup_by(|left, right| left.name == right.name);

    let deferred_tokens = definitions
        .iter()
        .filter(|tool| {
            let classification = classify(&tool.name);
            effective_enabled(configured, live, &tool.name)
                && !classification.essential
                && !pinned(settings, &tool.name)
                && !promoted.contains(&tool.name)
        })
        .map(estimated_tokens)
        .sum();
    // The search schema itself consumes context. Auto mode therefore uses
    // the net reduction rather than claiming the full deferred-schema cost
    // as savings. Explicit `always` honors the operator's choice even for a
    // tiny catalog; the reported savings still saturate at zero below.
    let loader_tokens = estimated_tokens(&load_tool_definition());
    let active = effective_enabled(configured, live, LOAD_TOOL_NAME)
        && lazy_active(
            settings.mode,
            settings.min_saved_tokens,
            context_window,
            deferred_tokens,
            loader_tokens,
        );

    let mut entries = Vec::with_capacity(definitions.len());
    let mut advertised = Vec::new();
    let mut total = 0_u64;
    let mut visible = 0_u64;
    for tool in definitions {
        let classification = classify(&tool.name);
        let enabled = effective_enabled(configured, live, &tool.name);
        let loaded = promoted.contains(&tool.name);
        let is_advertised = enabled
            && (!active || classification.essential || pinned(settings, &tool.name) || loaded);
        let estimate = estimated_tokens(&tool);
        if enabled {
            total = total.saturating_add(estimate);
        }
        if is_advertised {
            visible = visible.saturating_add(estimate);
            advertised.push(tool.clone());
        }
        entries.push(ToolCatalogEntry {
            id: tool.name.clone(),
            description: tool.description,
            group: classification.group.into(),
            group_title: classification.title.into(),
            source: tool_source(&tool.name),
            essential: classification.essential,
            enabled,
            advertised: is_advertised,
            loaded,
            estimated_tokens: estimate,
        });
    }
    if active {
        let loader = load_tool_definition();
        visible = visible.saturating_add(loader_tokens);
        advertised.push(loader);
    }
    ToolSelection {
        response: ToolCatalogResponse {
            tools: entries,
            lazy_mode: settings.mode,
            lazy_active: active,
            estimated_total_tokens: total,
            estimated_advertised_tokens: visible,
            estimated_saved_tokens: total.saturating_sub(visible),
        },
        advertised,
    }
}

/// Search only enabled, currently deferred tools. Exact id matches win;
/// wildcard and text matches are deterministic and bounded.
#[must_use]
pub fn search_deferred_tools(
    definitions: &[ToolDefinition],
    catalog: &ToolCatalogResponse,
    queries: &[String],
) -> Vec<String> {
    let by_id = definitions
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect::<HashMap<_, _>>();
    let deferred = catalog
        .tools
        .iter()
        .filter(|entry| entry.enabled && !entry.advertised && by_id.contains_key(entry.id.as_str()))
        .collect::<Vec<_>>();
    let mut matched = Vec::new();
    for raw in queries.iter().take(10) {
        let query = raw.trim().to_ascii_lowercase();
        if query.is_empty() {
            continue;
        }
        let mut candidates = deferred
            .iter()
            .filter(|entry| {
                if entry.id.eq_ignore_ascii_case(&query) {
                    return true;
                }
                if query.contains('*') && wildcard_matches(&query, &entry.id.to_ascii_lowercase()) {
                    return true;
                }
                let haystack = by_id.get(entry.id.as_str()).map_or_else(
                    || format!("{} {} {}", entry.id, entry.group, entry.source),
                    |tool| {
                        format!(
                            "{} {} {} {} {}",
                            entry.id,
                            entry.group,
                            entry.group_title,
                            entry.source,
                            tool.description
                        )
                    },
                );
                query
                    .split_whitespace()
                    .all(|word| haystack.to_ascii_lowercase().contains(word))
            })
            .map(|entry| entry.id.clone())
            .collect::<Vec<_>>();
        candidates.sort_by_key(|id| (!id.eq_ignore_ascii_case(&query), id.clone()));
        for id in candidates {
            if !matched.contains(&id) {
                matched.push(id);
                if matched.len() == MAX_LOAD_MATCHES {
                    return matched;
                }
            }
        }
    }
    matched
}

#[must_use]
pub fn load_tool_result(ids: &[String], catalog: &ToolCatalogResponse) -> Value {
    let tools = ids
        .iter()
        .filter_map(|id| catalog.tools.iter().find(|tool| tool.id == *id))
        .map(|tool| {
            json!({
                "id": tool.id,
                "group": tool.group,
                "description": tool.description,
                "estimatedTokens": tool.estimated_tokens,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "loaded": tools,
        "message": if ids.is_empty() {
            "No deferred tools matched. Try a functional term such as sessions, memory, extensions, automation, or an exact/wildcard tool id."
        } else {
            "The matching tool schemas will be available on the next model round."
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(name: &str, description: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.into(),
            description: description.into(),
            input_schema: json!({"type":"object","properties":{"query":{"type":"string"}}}),
        }
    }

    #[test]
    fn always_mode_defers_optional_schemas_and_keeps_core_resident() {
        let settings = LazyToolSettings {
            mode: LazyToolMode::Always,
            ..LazyToolSettings::default()
        };
        let selection = select_tools(
            vec![
                definition("read", "read a file"),
                definition("session_manage", "sessions"),
            ],
            &HashMap::new(),
            &HashMap::new(),
            &settings,
            &HashSet::new(),
            Some(128_000),
        );
        assert!(selection.response.lazy_active);
        assert!(selection.advertised.iter().any(|tool| tool.name == "read"));
        assert!(
            selection
                .advertised
                .iter()
                .any(|tool| tool.name == LOAD_TOOL_NAME)
        );
        assert!(
            !selection
                .advertised
                .iter()
                .any(|tool| tool.name == "session_manage")
        );
    }

    #[test]
    fn disabled_tools_are_neither_advertised_nor_searchable() {
        let definitions = vec![definition("memory_read", "read durable memory")];
        let settings = LazyToolSettings {
            mode: LazyToolMode::Always,
            ..LazyToolSettings::default()
        };
        let disabled = HashMap::from([("memory_read".into(), false)]);
        let selection = select_tools(
            definitions.clone(),
            &disabled,
            &HashMap::new(),
            &settings,
            &HashSet::new(),
            None,
        );
        assert!(selection.advertised.is_empty());
        assert!(
            search_deferred_tools(&definitions, &selection.response, &["memory".into()]).is_empty()
        );
    }

    #[test]
    fn search_supports_exact_wildcard_group_and_description() {
        let definitions = vec![
            definition("memory_read", "read durable project notes"),
            definition("session_manage", "manage sessions"),
            definition("mcp__github__pull_request", "inspect pull requests"),
        ];
        let settings = LazyToolSettings {
            mode: LazyToolMode::Always,
            ..LazyToolSettings::default()
        };
        let selection = select_tools(
            definitions.clone(),
            &HashMap::new(),
            &HashMap::new(),
            &settings,
            &HashSet::new(),
            None,
        );
        let found = search_deferred_tools(
            &definitions,
            &selection.response,
            &["memory".into(), "mcp__github__*".into()],
        );
        assert_eq!(found, ["memory_read", "mcp__github__pull_request"]);
    }

    #[test]
    fn auto_mode_counts_the_loader_cost_before_claiming_context_savings() {
        assert!(!lazy_active(
            LazyToolMode::Auto,
            512,
            Some(32_000),
            1_000,
            489,
        ));
        assert!(lazy_active(
            LazyToolMode::Auto,
            512,
            Some(32_000),
            1_001,
            489,
        ));
        assert!(lazy_active(LazyToolMode::Always, 512, None, 1, 10_000));
    }
}
