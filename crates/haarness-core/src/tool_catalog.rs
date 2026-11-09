//! Provider-neutral tool catalog, availability policy, and lazy schema selection.

use std::{
    collections::{HashMap, HashSet},
    hash::BuildHasher,
};

use haarness_protocol::{Item, ItemData, LazyToolMode, ToolCatalogEntry, ToolCatalogResponse};
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

/// Whether `model` comes from a family trained on `apply_patch`'s V4A
/// patches, whatever provider serves it (`openai/gpt-5.4` through
/// OpenRouter, `haarness/openai/gpt-6.1-sol`): OpenAI's GPT models from
/// GPT-4.1 on, its o-series reasoning models and its Codex models. Other
/// models change files with `edit`, `multiedit` and `write`, which every
/// model has; given the patch tool too they write diffs it cannot read.
/// gpt-oss writes the patch well but names its argument `patch`, not
/// `input`, call after call; a provider that checks calls against the tool
/// (Groq) refuses each one, so it edits as other models do.
#[must_use]
pub fn writes_patches(model: &str) -> bool {
    let name = model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_ascii_lowercase();
    let name = name.strip_prefix("openai.").unwrap_or(&name);
    if name.starts_with("gpt-oss") {
        return false;
    }
    if name.contains("codex") {
        return true;
    }
    if let Some(version) = name.strip_prefix("gpt-") {
        let major: String = version.chars().take_while(char::is_ascii_digit).collect();
        return match major.parse::<u32>() {
            Ok(major) if major >= 5 => true,
            Ok(4) => version.starts_with("4.1"),
            _ => false,
        };
    }
    // o1, o3, o4-mini, o3-pro.
    let mut characters = name.chars();
    characters.next() == Some('o') && characters.next().is_some_and(|c| c.is_ascii_digit())
}

/// The tool that runs a model's JavaScript (Code Mode).
pub const CODE_TOOL_NAME: &str = "execute";

/// Whether the JavaScript `execute` runs may call the tool `name`: only an
/// MCP server's tools and the browser's. Everything else (the shell, files,
/// the web) stays out of its reach.
#[must_use]
pub fn code_can_call(name: &str) -> bool {
    name.starts_with("mcp__") || name.starts_with("browser_")
}

/// `tools` without `execute` when none of them is one it can call: then it
/// can do nothing, and a model that reads its name as "run a command" (as
/// small local models do) calls it again and again instead of the shell.
#[must_use]
pub fn without_idle_code_tool(tools: Vec<ToolDefinition>) -> Vec<ToolDefinition> {
    if tools.iter().any(|tool| code_can_call(&tool.name)) {
        return tools;
    }
    tools
        .into_iter()
        .filter(|tool| tool.name != CODE_TOOL_NAME)
        .collect()
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
    } else if matches!(
        name,
        "cloud_agent_manage" | "cloud_workspace" | "cloud_chat" | "cloud_schedule"
    ) {
        ("cloud", "Cloud agents", false)
    } else if name.starts_with("session_") {
        ("sessions", "Sessions", false)
    } else if matches!(name, "workflow" | "schedule") {
        ("automation", "Automation", false)
    } else if name == "webfetch" || name.starts_with("web_") {
        ("web", "Web", true)
    } else if name.starts_with("mac_") {
        // Offered only while computer use is on; then they are the point.
        ("computer", "Computer use", true)
    } else if matches!(name, "image_generate" | "video_generate") {
        ("media", "Media", false)
    } else if name.starts_with("ext__")
        || name.starts_with("mcp__")
        || matches!(
            name,
            "mcp_manage"
                | "mcp_resource_list"
                | "mcp_resource_read"
                | "plugin_manage"
                | "skill_manage"
                | "command_manage"
                // Installing from the marketplace, and turning installed
                // capabilities on or off, as the other managers: only when
                // asked. Resident, they were 4.5 KB of every request.
                | "marketplace_manage"
                | "capability_manage"
                | "skill"
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

/// The window at or under which a chat leaves the browser's tools out until
/// the model loads them, as it does other optional groups. Chrome's and
/// Desktop's browser take some 45 schemas, about 9K tokens: a quarter of a
/// local model's 32K, which Haarness's other instructions and tools already
/// share with the chat, for tools a small model seldom needs.
pub const SMALL_WINDOW: u64 = 65_536;

/// The window at or under which only the tools that read, search, change and
/// run the project go with every request, and every other tool waits for
/// the model to load it: a model with an 8K or 16K window, or an account
/// whose plan allows fewer tokens a minute than one request with every tool
/// (Groq's free tier: 8,000), where the others were half of each request.
pub const TIGHT_WINDOW: u64 = 16_384;

/// The tools a chat keeps in a tight window.
const TIGHT_RESIDENT: [&str; 8] = [
    "read",
    "list",
    "glob",
    "grep",
    "edit",
    "write",
    "apply_patch",
    "bash",
];

/// [`classify`] for a model with `context_window`: the browser's tools are
/// optional in a small window, and all but the project's own in a tight one.
fn classify_in(name: &str, context_window: Option<u64>) -> Classification {
    let window = context_window.filter(|window| *window > 0);
    if name.starts_with("browser_") && window.is_some_and(|window| window <= SMALL_WINDOW) {
        return Classification {
            group: "browser",
            title: "Browser",
            essential: false,
        };
    }
    let classification = classify(name);
    // Computer use's tools are offered only while it is on; then they are
    // the point.
    if window.is_some_and(|window| window <= TIGHT_WINDOW)
        && !TIGHT_RESIDENT.contains(&name)
        && !name.starts_with("mac_")
    {
        return Classification {
            essential: false,
            ..classification
        };
    }
    classification
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
/// that floor (never below 512 tokens). Missing context metadata, or a
/// window listed as 0, leaves the full catalog visible; operators can
/// explicitly select `always`.
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
        LazyToolMode::Auto => context_window
            .filter(|window| *window > 0)
            .is_some_and(|window| {
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
/// `deferring`: Auto deferred tools for this chat and model before, so it
/// goes on deferring them, whatever has changed since (a server joining or
/// leaving, the model's window learned, the model loading tools): the tools
/// open the cached prompt, and the whole list changing would cost the chat
/// its cache. A chat sent every tool can still switch to deferring once, as
/// sending every schema could crowd a small model's window.
#[must_use]
pub fn select_tools<ConfiguredHasher, LiveHasher, PromotedHasher>(
    mut definitions: Vec<ToolDefinition>,
    configured: &HashMap<String, bool, ConfiguredHasher>,
    live: &HashMap<String, bool, LiveHasher>,
    settings: &LazyToolSettings,
    promoted: &HashSet<String, PromotedHasher>,
    context_window: Option<u64>,
    deferring: bool,
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
            let classification = classify_in(&tool.name, context_window);
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
        && ((deferring && settings.mode == LazyToolMode::Auto)
            || lazy_active(
                settings.mode,
                settings.min_saved_tokens,
                context_window,
                deferred_tokens,
                loader_tokens,
            ));

    let mut entries = Vec::with_capacity(definitions.len());
    let mut advertised = Vec::new();
    let mut total = 0_u64;
    let mut visible = 0_u64;
    for tool in definitions {
        let classification = classify_in(&tool.name, context_window);
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
    let deferred = deferred_entries(catalog, &by_id);
    let mut matched = Vec::new();
    for raw in queries.iter().take(10) {
        let query = raw.trim().to_ascii_lowercase();
        if query.is_empty() {
            continue;
        }
        for id in query_matches(&deferred, &by_id, &query) {
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

/// The tools a query names exactly, or by a wildcard, among those the model
/// has this round, for each query that names no deferred tool: loading them
/// does nothing, and a small model told that nothing matched tries the same
/// id again. Bounded as a search is.
#[must_use]
pub fn already_available(
    definitions: &[ToolDefinition],
    catalog: &ToolCatalogResponse,
    queries: &[String],
) -> Vec<String> {
    let by_id = definitions
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect::<HashMap<_, _>>();
    let deferred = deferred_entries(catalog, &by_id);
    let mut found = Vec::new();
    for raw in queries.iter().take(10) {
        let query = raw.trim().to_ascii_lowercase();
        if query.is_empty() || !query_matches(&deferred, &by_id, &query).is_empty() {
            continue;
        }
        for entry in catalog.tools.iter().filter(|entry| entry.advertised) {
            let named = entry.id.eq_ignore_ascii_case(&query)
                || (query.contains('*')
                    && wildcard_matches(&query, &entry.id.to_ascii_lowercase()));
            if named && !found.contains(&entry.id) {
                found.push(entry.id.clone());
                if found.len() == MAX_LOAD_MATCHES {
                    return found;
                }
            }
        }
    }
    found
}

fn deferred_entries<'a>(
    catalog: &'a ToolCatalogResponse,
    by_id: &HashMap<&str, &ToolDefinition>,
) -> Vec<&'a ToolCatalogEntry> {
    catalog
        .tools
        .iter()
        .filter(|entry| entry.enabled && !entry.advertised && by_id.contains_key(entry.id.as_str()))
        .collect()
}

/// The deferred tools one (lowercased) query matches, exact ids first.
fn query_matches(
    deferred: &[&ToolCatalogEntry],
    by_id: &HashMap<&str, &ToolDefinition>,
    query: &str,
) -> Vec<String> {
    let mut candidates = deferred
        .iter()
        .filter(|entry| {
            if entry.id.eq_ignore_ascii_case(query) {
                return true;
            }
            if query.contains('*') && wildcard_matches(query, &entry.id.to_ascii_lowercase()) {
                return true;
            }
            let haystack = by_id.get(entry.id.as_str()).map_or_else(
                || format!("tool tools {} {} {}", entry.id, entry.group, entry.source),
                |tool| {
                    format!(
                        "tool tools {} {} {} {} {}",
                        entry.id, entry.group, entry.group_title, entry.source, tool.description
                    )
                },
            );
            query
                .split_whitespace()
                .all(|word| haystack.to_ascii_lowercase().contains(word))
        })
        .map(|entry| entry.id.clone())
        .collect::<Vec<_>>();
    candidates.sort_by_key(|id| (!id.eq_ignore_ascii_case(query), id.clone()));
    candidates
}

/// What `load_tool` answers: the tools it `loaded`, those the model
/// `already` has, and matches it left out because the chat holds
/// [`MAX_PROMOTED_TOOLS`] loaded tools (`full`).
#[must_use]
pub fn load_tool_result(
    loaded: &[String],
    already: &[String],
    full: &[String],
    catalog: &ToolCatalogResponse,
) -> Value {
    let tools = loaded
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
    let mut message = Vec::new();
    if !loaded.is_empty() {
        message.push(
            "The matching tool schemas will be available on the next model round.".to_owned(),
        );
    }
    if !already.is_empty() {
        message.push(format!(
            "Already in your tool list, so there is nothing to load: {}. Call {} directly.",
            already.join(", "),
            if already.len() == 1 { "it" } else { "them" }
        ));
    }
    if !full.is_empty() {
        message.push(format!(
            "Not loaded, because this session already holds the most loaded tools it can ({MAX_PROMOTED_TOOLS}): {}. Use the tools you have.",
            full.join(", ")
        ));
    }
    if message.is_empty() {
        message.push("No deferred tools matched. Try a functional term such as sessions, memory, extensions, automation, or an exact/wildcard tool id.".to_owned());
    }
    let mut value = json!({
        "loaded": tools,
        "message": message.join(" "),
    });
    if !already.is_empty() {
        value["available"] = json!(already);
    }
    if !full.is_empty() {
        value["notLoaded"] = json!(full);
    }
    value
}

/// The tools a chat's history shows promoted, as the chat held them while
/// it ran: those `load_tool` loaded and the optional tools the model called,
/// a `batch` call's steps included, in the order the calls were made and at
/// most [`MAX_PROMOTED_TOOLS`]. A call counts whatever its outcome, as its
/// tool was promoted when it started, unless it never started: its result
/// says so (`notRun`, see [`crate::not_run_metadata`]) or reads as a refusal
/// made before the start (see [`refused_before_running`]). A call with no
/// result does not count either.
/// For a process that has no record of the chat's loads (after a restart, or
/// in a fork, whose history holds its source's). `tools` are the registered
/// names, which a called name is read as. Code Mode's calls are not in the
/// history, so the tools only they reached are not read back.
#[must_use]
pub fn promoted_in(items: &[Item], tools: &[String]) -> HashSet<String> {
    let registered = tools.iter().map(String::as_str).collect::<HashSet<_>>();
    let resolve = |name: &str| {
        if name == LOAD_TOOL_NAME || registered.contains(name) {
            name.to_owned()
        } else {
            crate::tool_validation::resolve_name(
                name,
                tools
                    .iter()
                    .cloned()
                    .chain(std::iter::once(LOAD_TOOL_NAME.to_owned())),
            )
        }
    };
    let mut promoted = HashSet::new();
    let mut promote = |name: String| {
        if promoted.len() < MAX_PROMOTED_TOOLS {
            promoted.insert(name);
        }
    };
    for (call, result) in crate::pair_tool_calls(items.iter().map(|item| &item.data)).answered {
        let (
            ItemData::ToolCall { name, .. },
            ItemData::ToolResult {
                output,
                is_error,
                metadata,
                ..
            },
        ) = (&items[call].data, &items[result].data)
        else {
            continue;
        };
        if crate::not_run(metadata) || (*is_error && refused_before_running(output)) {
            continue;
        }
        let name = resolve(name);
        if name == LOAD_TOOL_NAME {
            if !is_error {
                metadata
                    .get("loaded")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|tool| tool.get("id").and_then(Value::as_str))
                    .for_each(|id| promote(id.to_owned()));
            }
        } else if name == "batch" {
            let steps = metadata
                .get("results")
                .and_then(Value::as_array)
                .into_iter()
                .flatten();
            for step in steps {
                let refused = step
                    .get("error")
                    .and_then(Value::as_str)
                    .is_some_and(refused_before_running);
                if let Some(step) = step.get("tool").and_then(Value::as_str)
                    && !refused
                {
                    let step = resolve(step);
                    if !is_essential_tool(&step) {
                        promote(step);
                    }
                }
            }
        } else if !is_essential_tool(&name) {
            promote(name);
        }
    }
    promoted
}

/// Whether a call's error is a refusal made before the call started, which
/// promoted nothing: an unknown tool, one deferred, not offered or turned off
/// for the round, one an inherited rule denies, or arguments its schema
/// refuses. Reads the runtime's wording of those refusals.
fn refused_before_running(error: &str) -> bool {
    let line = error.lines().next().unwrap_or_default();
    let line = ["tool error: ", "permission denied: ", "invalid request: "]
        .iter()
        .find_map(|category| line.strip_prefix(category))
        .unwrap_or(line);
    line.starts_with("unknown tool: ")
        || (line.starts_with("tool ")
            && [
                " is deferred and was not advertised",
                " was not advertised for this provider round",
                " is disabled for this workflow agent turn",
                " is denied by the rule '",
            ]
            .iter()
            .any(|refusal| line.contains(refusal)))
        || (line.starts_with("The ") && line.contains(" tool was called with invalid arguments"))
}

#[cfg(test)]
mod tests {
    use super::writes_patches;

    #[test]
    fn the_patch_tool_goes_to_the_model_families_trained_on_it_under_any_provider() {
        for model in [
            "gpt-5.4",
            "gpt-6-luna",
            "openai/gpt-6.1-sol",
            "haarness/openai/gpt-6.1-sol",
            "gpt-4.1-mini",
            "o3",
            "o4-mini",
            "gpt-5-codex",
            "codex-mini-latest",
            "GPT-5",
        ] {
            assert!(writes_patches(model), "{model}");
        }
        for model in [
            "claude-sonnet-5-5",
            "anthropic/claude-opus-5-5",
            "gemini-3.8-flash",
            "deepseek-flash",
            "gpt-4o",
            "gpt-4",
            "gpt-3.5-turbo",
            "grok-4.3",
            "qwen3-coder",
            "my-deployment",
            "ollama/llama4",
            "openrouter/auto",
        ] {
            assert!(!writes_patches(model), "{model}");
        }
    }

    /// gpt-oss names apply_patch's argument `patch`, and Groq refuses every
    /// such call (four in four for the 120B model): it edits instead,
    /// under any provider and however the id is written.
    #[test]
    fn gpt_oss_changes_files_with_edit_and_write_not_the_patch_tool() {
        for model in [
            "gpt-oss-120b",
            "openai/gpt-oss-120b",
            "openai/gpt-oss-20b",
            "openai/gpt-oss-safeguard-20b",
            "openai.gpt-oss-120b-1:0",
            "gpt-oss:120b",
            "gpt-oss:20b-cloud",
            "haarness/openai/gpt-oss-120b",
            "GPT-OSS-120B",
        ] {
            assert!(!writes_patches(model), "{model}");
        }
    }

    #[test]
    fn the_marketplace_and_capability_managers_wait_to_be_loaded_like_the_other_managers() {
        for name in [
            "marketplace_manage",
            "capability_manage",
            "skill_manage",
            "mcp_manage",
        ] {
            let kind = super::classify(name);
            assert_eq!(kind.group, "extensions", "{name}");
            assert!(!kind.essential, "{name}");
        }
        for name in ["read", "edit", "apply_patch", "bash", "grep", "glob"] {
            assert!(super::classify(name).essential, "{name}");
        }
    }

    #[test]
    fn cloud_tools_are_deferred_in_the_cloud_group() {
        for name in ["cloud_agent_manage", "cloud_workspace"] {
            let kind = super::classify(name);
            assert_eq!(kind.group, "cloud");
            assert!(!kind.essential);
        }
    }
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
            false,
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
    fn a_small_window_loads_the_browser_when_the_model_asks() {
        let definitions: Vec<_> = ["read", "bash", "browser_navigate", "browser_snapshot"]
            .into_iter()
            .map(|name| definition(name, &"a long description ".repeat(200)))
            .collect();
        let select = |window| {
            select_tools(
                definitions.clone(),
                &HashMap::new(),
                &HashMap::new(),
                &LazyToolSettings::default(),
                &HashSet::new(),
                Some(window),
                false,
            )
        };
        // A local model's 32K: the browser waits behind the loader.
        let small = select(32_768);
        let names: Vec<_> = small
            .advertised
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert_eq!(names, ["bash", "read", LOAD_TOOL_NAME]);
        assert_eq!(
            search_deferred_tools(&definitions, &small.response, &["browser".into()]),
            ["browser_navigate", "browser_snapshot"]
        );
        // Once loaded, it stays.
        let loaded = select_tools(
            definitions.clone(),
            &HashMap::new(),
            &HashMap::new(),
            &LazyToolSettings::default(),
            &HashSet::from(["browser_navigate".to_owned()]),
            Some(32_768),
            true,
        );
        assert!(
            loaded
                .advertised
                .iter()
                .any(|tool| tool.name == "browser_navigate")
        );
        // A large window keeps it at hand, as before.
        let large = select(200_000);
        assert!(
            large
                .advertised
                .iter()
                .any(|tool| tool.name == "browser_snapshot")
        );
        assert!(!large.response.lazy_active);
    }

    /// An 8K window, or an account allowing 8,000 tokens a minute: the tools
    /// that read, search, change and run the project go with each request,
    /// and the rest wait behind the loader, which finds them.
    #[test]
    fn a_tight_window_keeps_only_the_projects_own_tools_at_hand() {
        let names = [
            "apply_patch",
            "bash",
            "bash_output",
            "bash_stop",
            "batch",
            "edit",
            "execute",
            "glob",
            "grep",
            "list",
            "lsp",
            "mac_snapshot",
            "multiedit",
            "question",
            "read",
            "task",
            "todowrite",
            "webfetch",
            "write",
        ];
        let definitions: Vec<_> = names
            .into_iter()
            .map(|name| definition(name, &"a description of what it does ".repeat(20)))
            .collect();
        let select = |window, promoted: &[&str]| {
            select_tools(
                definitions.clone(),
                &HashMap::new(),
                &HashMap::new(),
                &LazyToolSettings::default(),
                &promoted
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect::<HashSet<_>>(),
                Some(window),
                false,
            )
        };
        let advertised = |selection: &ToolSelection| {
            selection
                .advertised
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<Vec<_>>()
        };
        let tight = select(TIGHT_WINDOW, &[]);
        assert_eq!(
            advertised(&tight),
            [
                "apply_patch",
                "bash",
                "edit",
                "glob",
                "grep",
                "list",
                "mac_snapshot",
                "read",
                "write",
                LOAD_TOOL_NAME
            ]
        );
        assert!(tight.response.estimated_saved_tokens > 1_000);
        assert_eq!(
            search_deferred_tools(&definitions, &tight.response, &["task".into()]),
            ["task"]
        );
        // A tool the model loaded stays.
        assert!(advertised(&select(8_000, &["todowrite"])).contains(&"todowrite".to_owned()));
        // A window a little larger, as a local model's 32K, keeps them all.
        let small = select(32_768, &[]);
        assert_eq!(small.advertised.len(), names.len());
        assert!(!small.response.lazy_active);
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
            false,
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
            false,
        );
        let found = search_deferred_tools(
            &definitions,
            &selection.response,
            &["memory".into(), "mcp__github__*".into()],
        );
        assert_eq!(found, ["memory_read", "mcp__github__pull_request"]);
    }

    #[test]
    fn natural_tool_queries_find_skills_without_tool_in_the_description() {
        let definitions = vec![
            definition("skill", "Load a specialized Agent Skill"),
            definition("mcp__docs__search", "Search documentation"),
        ];
        let selection = select_tools(
            definitions.clone(),
            &HashMap::new(),
            &HashMap::new(),
            &LazyToolSettings {
                mode: LazyToolMode::Always,
                ..LazyToolSettings::default()
            },
            &HashSet::new(),
            None,
            false,
        );
        for query in ["skill tool", "skill tools", "skill"] {
            assert_eq!(
                search_deferred_tools(&definitions, &selection.response, &[query.into()]),
                ["skill"]
            );
        }
        assert_eq!(
            search_deferred_tools(
                &definitions,
                &selection.response,
                &["documentation tool".into()]
            ),
            ["mcp__docs__search"]
        );
    }

    fn names(tools: &[ToolDefinition]) -> Vec<&str> {
        tools.iter().map(|tool| tool.name.as_str()).collect()
    }

    #[test]
    fn a_chat_auto_deferred_for_keeps_deferring_when_the_savings_shrink() {
        let definitions = vec![
            definition("read", "read a file"),
            definition("memory_read", &"durable project notes ".repeat(500)),
            definition("session_manage", "sessions"),
        ];
        let select = |promoted: &HashSet<String>, deferring: bool, mode: LazyToolMode| {
            select_tools(
                definitions.clone(),
                &HashMap::new(),
                &HashMap::new(),
                &LazyToolSettings {
                    mode,
                    ..LazyToolSettings::default()
                },
                promoted,
                Some(200_000),
                deferring,
            )
        };
        let first = select(&HashSet::new(), false, LazyToolMode::Auto);
        assert!(first.response.lazy_active);
        assert_eq!(names(&first.advertised), ["read", LOAD_TOOL_NAME]);

        // Once the model loaded the large schema, what is left to defer no
        // longer pays for the loader: decided afresh, every tool would be
        // sent and the loader dropped, a new tool list mid-chat.
        let loaded = HashSet::from(["memory_read".to_owned()]);
        let afresh = select(&loaded, false, LazyToolMode::Auto);
        assert!(!afresh.response.lazy_active);
        let kept = select(&loaded, true, LazyToolMode::Auto);
        assert!(kept.response.lazy_active);
        assert_eq!(
            names(&kept.advertised),
            ["memory_read", "read", LOAD_TOOL_NAME]
        );

        // The person's own choice of mode is not overruled.
        assert!(
            !select(&HashSet::new(), true, LazyToolMode::Off)
                .response
                .lazy_active
        );
        // Nor is turning the loader off.
        let without_loader = select_tools(
            definitions.clone(),
            &HashMap::from([(LOAD_TOOL_NAME.to_owned(), false)]),
            &HashMap::new(),
            &LazyToolSettings::default(),
            &loaded,
            Some(200_000),
            true,
        );
        assert!(!without_loader.response.lazy_active);
    }

    fn item(data: ItemData) -> Item {
        Item {
            id: haarness_protocol::ItemId::new(),
            thread_id: haarness_protocol::ThreadId::new(),
            turn_id: None,
            sequence: 0,
            data,
            created_at: chrono::Utc::now(),
        }
    }

    fn call(id: &str, name: &str) -> Item {
        item(ItemData::ToolCall {
            call_id: id.into(),
            name: name.into(),
            arguments: json!({}),
            provider_metadata: Value::Null,
        })
    }

    fn answer(id: &str, name: &str, output: &str, is_error: bool, metadata: Value) -> Item {
        item(ItemData::ToolResult {
            call_id: id.into(),
            name: name.into(),
            output: output.into(),
            is_error,
            metadata,
        })
    }

    /// A call and its result.
    fn ran(id: &str, name: &str, is_error: bool, metadata: Value) -> [Item; 2] {
        [call(id, name), answer(id, name, "", is_error, metadata)]
    }

    fn loads(ids: &[&str]) -> Value {
        json!({
            "loaded": ids.iter().map(|id| json!({"id": id})).collect::<Vec<_>>(),
            "message": "The matching tool schemas will be available on the next model round.",
        })
    }

    #[test]
    fn a_chats_loads_and_calls_are_read_back_from_its_history() {
        let tools = [
            "read",
            "memory_read",
            "memory_save",
            "mcp__docs__search",
            "session_manage",
            "skill",
            "cloud_chat",
            "workflow",
        ]
        .map(String::from);
        let refused = |id: &str, name: &str, error: &str| {
            [call(id, name), answer(id, name, error, true, Value::Null)]
        };
        let items = [
            ran("1", LOAD_TOOL_NAME, false, loads(&["mcp__docs__search"])),
            // Called while every tool was offered.
            ran("2", "Memory_Read", false, Value::Null),
            // It ran and failed, or a person rejected it: it was promoted
            // when it started all the same.
            ran("3", "memory_save", true, Value::Null),
            // Core tools are always offered; nothing to keep.
            ran("4", "read", false, Value::Null),
            // Refused before it could start: deferred and not loaded.
            refused(
                "5",
                "session_manage",
                "tool error: tool session_manage is deferred and was not advertised for this provider round. Call load_tool …",
            ),
            refused("6", "skill", "invalid request: The skill tool was called with invalid arguments. Fix the input and retry the same tool."),
            refused("7", "nope", "tool error: unknown tool: nope. Use an exact available tool name."),
            ran("8", LOAD_TOOL_NAME, true, Value::Null),
            // Never started: its round broke off, Stop came first, or a
            // restart stopped the batch before it.
            ran("8a", "session_manage", true, json!({"notRun": true})),
            ran(
                "8b",
                "skill",
                true,
                json!({"unfinished": true, "notRun": true}),
            ),
            // The call a restart cut off while it ran.
            ran("8c", "workflow", true, json!({"unfinished": true})),
            // A batch's steps, the refused one left out.
            ran(
                "9",
                "batch",
                false,
                json!({"results": [
                    {"tool": "cloud_chat", "output": "ok"},
                    {"tool": "read", "output": "ok"},
                    {"tool": "session_manage", "error": "permission denied: tool session_manage is disabled for this workflow agent turn or by permission policy."},
                ], "failures": 1}),
            ),
        ]
        .into_iter()
        .flatten()
        .chain([call("10", "memory_read")]) // cut off: no result
        .collect::<Vec<_>>();
        assert_eq!(
            promoted_in(&items, &tools),
            HashSet::from([
                "mcp__docs__search".to_owned(),
                "memory_read".to_owned(),
                "memory_save".to_owned(),
                "cloud_chat".to_owned(),
                "workflow".to_owned(),
            ])
        );
    }

    #[test]
    fn the_rebuild_keeps_the_oldest_calls_by_when_they_were_made() {
        let many = (0..MAX_PROMOTED_TOOLS + 6)
            .map(|index| format!("mcp__many__tool_{index:03}"))
            .collect::<Vec<_>>();
        let mut items = many
            .chunks(10)
            .enumerate()
            .flat_map(|(index, chunk)| {
                let ids = chunk.iter().map(String::as_str).collect::<Vec<_>>();
                ran(&format!("load-{index}"), LOAD_TOOL_NAME, false, loads(&ids))
            })
            .collect::<Vec<_>>();
        let promoted = promoted_in(&items, &many);
        assert_eq!(promoted.len(), MAX_PROMOTED_TOOLS);
        assert!(promoted.contains(&many[MAX_PROMOTED_TOOLS - 1]));
        assert!(
            !promoted.contains(&many[MAX_PROMOTED_TOOLS]),
            "the oldest loads are kept"
        );

        // Two calls made together whose results came back the other way
        // round: the earlier call takes the last place.
        let tools = ["mcp__a__first", "mcp__a__second"].map(String::from);
        let full = (0..MAX_PROMOTED_TOOLS - 1)
            .map(|index| format!("mcp__full__tool_{index:03}"))
            .collect::<Vec<_>>();
        items = ran(
            "load",
            LOAD_TOOL_NAME,
            false,
            loads(&full.iter().map(String::as_str).collect::<Vec<_>>()),
        )
        .into_iter()
        .chain([
            call("first", "mcp__a__first"),
            call("second", "mcp__a__second"),
            answer("second", "mcp__a__second", "", false, Value::Null),
            answer("first", "mcp__a__first", "", false, Value::Null),
        ])
        .collect();
        let promoted = promoted_in(&items, &tools);
        assert!(promoted.contains("mcp__a__first") && !promoted.contains("mcp__a__second"));
    }

    #[test]
    fn loading_a_tool_already_offered_says_it_is_there() {
        let definitions = vec![
            definition("bash", "run a command"),
            definition("memory_read", "read durable notes"),
            definition("mcp__github__issues", "list issues"),
        ];
        let selection = select_tools(
            definitions.clone(),
            &HashMap::new(),
            &HashMap::new(),
            &LazyToolSettings {
                mode: LazyToolMode::Always,
                ..LazyToolSettings::default()
            },
            &HashSet::from(["mcp__github__issues".to_owned()]),
            None,
            false,
        );
        let catalog = &selection.response;
        let queries =
            ["BASH", "memory_read", "mcp__github__*", "nothing like it"].map(String::from);
        assert_eq!(
            search_deferred_tools(&definitions, catalog, &queries),
            ["memory_read"]
        );
        assert_eq!(
            already_available(&definitions, catalog, &queries),
            ["bash", "mcp__github__issues"]
        );
        // A text search is no claim about one tool.
        assert!(already_available(&definitions, catalog, &["run a command".into()]).is_empty());

        let only = load_tool_result(&[], &["bash".to_owned()], &[], catalog);
        assert_eq!(only["loaded"], json!([]));
        assert_eq!(only["available"], json!(["bash"]));
        let message = only["message"].as_str().unwrap();
        assert!(
            message.contains("Already in your tool list") && message.contains("bash"),
            "{message}"
        );
        assert!(!message.contains("No deferred tools matched"), "{message}");

        let both = load_tool_result(
            &["memory_read".to_owned()],
            &["bash".to_owned(), "mcp__github__issues".to_owned()],
            &[],
            catalog,
        );
        assert_eq!(both["loaded"][0]["id"], "memory_read");
        assert!(
            both["message"]
                .as_str()
                .unwrap()
                .contains("Call them directly")
        );
    }

    #[test]
    fn a_load_past_the_limit_says_so() {
        let definitions = vec![definition("memory_read", "read durable notes")];
        let selection = select_tools(
            definitions,
            &HashMap::new(),
            &HashMap::new(),
            &LazyToolSettings {
                mode: LazyToolMode::Always,
                ..LazyToolSettings::default()
            },
            &HashSet::new(),
            None,
            false,
        );
        let value = load_tool_result(&[], &[], &["memory_read".to_owned()], &selection.response);
        assert_eq!(value["notLoaded"], json!(["memory_read"]));
        let message = value["message"].as_str().unwrap();
        assert!(
            message.contains(&MAX_PROMOTED_TOOLS.to_string()),
            "{message}"
        );
        assert!(!message.contains("No deferred tools matched"), "{message}");

        // What a miss always said, unchanged.
        assert_eq!(
            load_tool_result(&[], &[], &[], &selection.response),
            json!({
                "loaded": [],
                "message": "No deferred tools matched. Try a functional term such as sessions, memory, extensions, automation, or an exact/wildcard tool id.",
            })
        );
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

    #[test]
    fn auto_mode_reads_a_window_of_0_as_unknown() {
        // As small a window as there is would defer these schemas.
        assert!(lazy_active(LazyToolMode::Auto, 4_000, Some(1), 600, 50));
        assert!(!lazy_active(LazyToolMode::Auto, 4_000, None, 600, 50));
        assert!(!lazy_active(LazyToolMode::Auto, 4_000, Some(0), 600, 50));
    }
}
