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
    } else if name.starts_with("browser_") {
        // Offered only while a browser can serve the chat. The few that
        // open, read and work a page stay; the rest wait to be loaded.
        ("browser", "Browser", BROWSER_RESIDENT.contains(&name))
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

/// The browser tools a chat keeps while lazy loading is on, above
/// [`SMALL_WINDOW`]: enough to open a page, read it, click, type and see
/// it, about 1.5K tokens. The browser's other 38 tools (some 7K tokens of
/// tabs, frames, files, network, profiling and more) wait to be loaded.
const BROWSER_RESIDENT: [&str; 6] = [
    "browser_tabs_open",
    "browser_navigate",
    "browser_snapshot",
    "browser_click",
    "browser_fill",
    "browser_screenshot",
];

/// The window at or under which a chat leaves all the browser's tools out
/// until the model loads them, as it does other optional groups. Chrome's
/// and Desktop's browser take some 45 schemas, about 9K tokens: a quarter of
/// a local model's 32K, which Haarness's other instructions and tools
/// already share with the chat, for tools a small model seldom needs.
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

fn tight(context_window: Option<u64>) -> bool {
    context_window.is_some_and(|window| window > 0 && window <= TIGHT_WINDOW)
}

/// [`classify`] for a model with `context_window`: all the browser's tools
/// are optional in a small window, and all but the project's own in a tight
/// one.
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
    if tight(context_window) && !TIGHT_RESIDENT.contains(&name) && !name.starts_with("mac_") {
        return Classification {
            essential: false,
            ..classification
        };
    }
    classification
}

/// [`classify_in`] for `tool` itself: the skill tool goes with every
/// request while it lists skills, as the list is the only place the model
/// learns that they exist (deferred, the tool and its list wait behind a
/// loader whose description names neither). Without skills it waits like
/// the other optional tools, and so it does in a tight window.
fn classify_tool(tool: &ToolDefinition, context_window: Option<u64>) -> Classification {
    let classification = classify_in(&tool.name, context_window);
    if lists_skills(tool) && !tight(context_window) {
        return Classification {
            essential: true,
            ..classification
        };
    }
    classification
}

/// The tool that loads a skill by its name. Its description ends with the
/// list [`skill_list`] writes.
pub const SKILL_TOOL_NAME: &str = "skill";

/// The words the skill list opens with.
const SKILL_LIST_OPENING: &str = " Available skills:";

/// The words before the skills the list names without a description.
const SKILL_NAMES_OPENING: &str = " More skills:";

/// What follows the names when some are only counted: how the model finds
/// one of those.
const SKILL_NAMES_ROUTE: &str =
    " Called with an unknown name, the skill tool answers with the closest ones.";

/// The most bytes the skill list may take (about 2K tokens). A longer list
/// is made to fit: shorter descriptions, then names alone, then a count.
pub const SKILL_LIST_BUDGET: usize = 8 * 1024;

/// The most characters of a skill's description a list over
/// [`SKILL_LIST_BUDGET`] keeps.
pub const SKILL_DESCRIPTION_LIMIT: usize = 250;

/// Whether `tool` is the skill tool with at least one skill in its list.
fn lists_skills(tool: &ToolDefinition) -> bool {
    tool.name == SKILL_TOOL_NAME
        && tool
            .description
            .split_once(SKILL_LIST_OPENING)
            .is_some_and(|(_, skills)| !skills.is_empty())
}

/// The list of `skills` (each a name and a description, in the order given)
/// the skill tool's description ends with. A list that fits
/// [`SKILL_LIST_BUDGET`] holds every description whole. A longer one, which
/// would make every request pay for many skills or very long descriptions,
/// puts the person's own skills ahead of plugins' (named `plugin:skill`),
/// shortens each description to [`SKILL_DESCRIPTION_LIMIT`] characters,
/// describes the skills in that order while they fit, and names the rest
/// in at most half the budget. Names that would not fit there are only
/// counted, and the model is told how to reach those skills.
#[must_use]
pub fn skill_list(skills: &[(&str, &str)]) -> String {
    use std::fmt::Write as _;
    let mut whole = String::from(SKILL_LIST_OPENING);
    for (name, description) in skills {
        let _ = write!(whole, " {name} ({description}).");
    }
    if whole.len() <= SKILL_LIST_BUDGET {
        return whole;
    }
    // Stable: each group keeps the order given.
    let mut ranked = skills.iter().collect::<Vec<_>>();
    ranked.sort_by_key(|(name, _)| name.contains(':'));
    let described = ranked
        .iter()
        .map(|(name, description)| format!(" {name} ({}).", shortened(description)))
        .collect::<Vec<_>>();
    let mut sizes = vec![SKILL_LIST_OPENING.len(); ranked.len() + 1];
    for (index, entry) in described.iter().enumerate() {
        sizes[index + 1] = sizes[index] + entry.len();
    }
    let named = |count: usize| {
        let rest = ranked[count..]
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>();
        if rest.is_empty() {
            return String::new();
        }
        let room = SKILL_LIST_BUDGET / 2;
        // As `some_names` decides whether to count some.
        let route = if rest.join(", ").len() > room {
            SKILL_NAMES_ROUTE
        } else {
            ""
        };
        format!("{SKILL_NAMES_OPENING} {}.{route}", some_names(&rest, room))
    };
    // The most skills described that leave room to name the rest. None
    // described always fits, as the names take at most half the budget.
    let count = (1..=ranked.len())
        .rev()
        .find(|count| {
            sizes[*count] <= SKILL_LIST_BUDGET
                && sizes[*count] + named(*count).len() <= SKILL_LIST_BUDGET
        })
        .unwrap_or(0);
    let mut text = String::from(SKILL_LIST_OPENING);
    for entry in &described[..count] {
        text.push_str(entry);
    }
    text.push_str(&named(count));
    text
}

/// `names` joined by commas, as many as fit `room` bytes, and the count of
/// the others after them: `a, b and 3 more`.
#[must_use]
pub fn some_names(names: &[&str], room: usize) -> String {
    let all = names.join(", ");
    if all.len() <= room {
        return all;
    }
    let mut text = String::new();
    let mut shown = 0;
    for (index, name) in names.iter().enumerate() {
        let separator = if index == 0 { "" } else { ", " };
        // What it reads with this name in: the others counted after it.
        let more = format!(" and {} more", names.len() - index - 1);
        if text.len() + separator.len() + name.len() + more.len() > room {
            break;
        }
        text.push_str(separator);
        text.push_str(name);
        shown += 1;
    }
    if shown == 0 {
        return format!("{} more", names.len());
    }
    format!("{text} and {} more", names.len() - shown)
}

/// `description` cut to [`SKILL_DESCRIPTION_LIMIT`] characters, ending
/// with an ellipsis where it was cut.
pub(crate) fn shortened(description: &str) -> std::borrow::Cow<'_, str> {
    if description.chars().count() <= SKILL_DESCRIPTION_LIMIT {
        return description.into();
    }
    let kept = description
        .chars()
        .take(SKILL_DESCRIPTION_LIMIT - 1)
        .collect::<String>();
    format!("{}…", kept.trim_end()).into()
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
            let classification = classify_tool(tool, context_window);
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
        let classification = classify_tool(&tool, context_window);
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

/// The deferred tools one `load_tool` call matched: those it loads, at most
/// [`MAX_LOAD_MATCHES`], and the matches past that, which it leaves out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeferredMatches {
    pub found: Vec<String>,
    pub left_out: Vec<String>,
}

/// Search only enabled, currently deferred tools. A query that is a
/// deferred tool's exact id is found first, whatever query comes before
/// it, so a wildcard or a word in the same call cannot crowd that tool
/// out; wildcard and text matches follow, deterministic, in the order of
/// the queries. A query that is the id of a tool the model has this round
/// finds only the deferred tools whose ids begin or end with it
/// (`bash_output` and `bash_stop` for `bash`, `skill_manage` for `skill`),
/// not an MCP tool holding it inside its id (`mcp__github__list_issues` for
/// `list`) or every one whose description mentions it; [`already_available`]
/// names the tool it has. Past [`MAX_LOAD_MATCHES`], matches are left out
/// and returned as such, for the answer to name.
#[must_use]
pub fn find_deferred_tools(
    definitions: &[ToolDefinition],
    catalog: &ToolCatalogResponse,
    queries: &[String],
) -> DeferredMatches {
    let by_id = definitions
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect::<HashMap<_, _>>();
    let deferred = deferred_entries(catalog, &by_id);
    let queries = queries
        .iter()
        .take(10)
        .map(|raw| raw.trim().to_ascii_lowercase())
        .filter(|query| !query.is_empty())
        .collect::<Vec<_>>();
    let mut matched = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |id: String| {
        if seen.insert(id.clone()) {
            matched.push(id);
        }
    };
    for query in &queries {
        if let Some(entry) = deferred
            .iter()
            .find(|entry| entry.id.eq_ignore_ascii_case(query))
        {
            add(entry.id.clone());
        }
    }
    for query in &queries {
        let found = if offered(catalog, query).is_some() {
            let mut ids = deferred
                .iter()
                .filter(|entry| {
                    let id = entry.id.to_ascii_lowercase();
                    id.starts_with(query.as_str()) || id.ends_with(query.as_str())
                })
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>();
            ids.sort();
            ids
        } else {
            query_matches(&deferred, &by_id, query)
        };
        found.into_iter().for_each(&mut add);
    }
    let left_out = if matched.len() > MAX_LOAD_MATCHES {
        matched.split_off(MAX_LOAD_MATCHES)
    } else {
        Vec::new()
    };
    DeferredMatches {
        found: matched,
        left_out,
    }
}

/// The tools a query names exactly among those the model has this round,
/// and those a wildcard names for each query that names no deferred tool:
/// loading them does nothing, and a small model told that nothing matched
/// tries the same id again. Bounded as a search is.
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
        if let Some(id) = offered(catalog, &query) {
            if !found.iter().any(|known| known == id) {
                found.push(id.to_owned());
                if found.len() == MAX_LOAD_MATCHES {
                    return found;
                }
            }
            continue;
        }
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

/// The tool the model has this round whose id is `query` (lowercased).
fn offered<'a>(catalog: &'a ToolCatalogResponse, query: &str) -> Option<&'a str> {
    catalog
        .tools
        .iter()
        .find(|entry| entry.advertised && entry.id.eq_ignore_ascii_case(query))
        .map(|entry| entry.id.as_str())
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

/// The most bytes the names of the matches one call left out may take in
/// its answer (about 500 tokens).
const LEFT_OUT_ROOM: usize = 2 * 1024;

/// What `load_tool` answers: the tools it `loaded`, those the model
/// `already` has, matches it left out because the chat holds
/// [`MAX_PROMOTED_TOOLS`] loaded tools (`full`), and those past the
/// [`MAX_LOAD_MATCHES`] one call loads (`left_out`), named so the model can
/// load the ones it needs by id.
#[must_use]
pub fn load_tool_result(
    loaded: &[String],
    already: &[String],
    full: &[String],
    left_out: &[String],
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
    if !left_out.is_empty() {
        let names = left_out.iter().map(String::as_str).collect::<Vec<_>>();
        message.push(format!(
            "Also matched, past the {MAX_LOAD_MATCHES} tools one call loads, so not loaded: {}. Load the ones you need by exact id.",
            some_names(&names, LEFT_OUT_ROOM)
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
/// for the round, one an inherited rule denies, arguments its schema
/// refuses, or a call that failed the same way twice before in its turn. Reads the runtime's wording of those refusals.
fn refused_before_running(error: &str) -> bool {
    let line = error.lines().next().unwrap_or_default();
    let line = ["tool error: ", "permission denied: ", "invalid request: "]
        .iter()
        .find_map(|category| line.strip_prefix(category))
        .unwrap_or(line);
    line.starts_with("unknown tool: ")
        || line.starts_with("Not run: this exact call already failed")
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

    /// [`find_deferred_tools`] without the matches it left out.
    fn search_deferred_tools(
        definitions: &[ToolDefinition],
        catalog: &ToolCatalogResponse,
        queries: &[String],
    ) -> Vec<String> {
        find_deferred_tools(definitions, catalog, queries).found
    }

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

    /// While lazy loading is on, a large window keeps the browser tools that
    /// open, read and work a page, and the others wait in the browser group:
    /// asked for the group, the loader loads twenty and names the rest, and
    /// one called by name is read back from the history after a restart.
    #[test]
    fn a_large_window_keeps_a_few_browser_tools_and_defers_the_rest() {
        let mut definitions = BROWSER_RESIDENT
            .iter()
            .map(|name| definition(name, &"a browser tool ".repeat(20)))
            .collect::<Vec<_>>();
        definitions.extend((0..30).map(|index| {
            definition(
                &format!("browser_more_{index:02}"),
                &"another browser tool ".repeat(20),
            )
        }));
        definitions.push(definition("read", "read a file"));
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
        let large = select(200_000);
        assert!(large.response.lazy_active);
        assert_eq!(
            names(&large.advertised),
            [
                "browser_click",
                "browser_fill",
                "browser_navigate",
                "browser_screenshot",
                "browser_snapshot",
                "browser_tabs_open",
                "read",
                LOAD_TOOL_NAME
            ]
        );
        let matches = find_deferred_tools(&definitions, &large.response, &["browser".into()]);
        assert_eq!(matches.found.len(), MAX_LOAD_MATCHES);
        assert_eq!(matches.left_out.len(), 10);
        assert!(
            matches
                .found
                .iter()
                .chain(&matches.left_out)
                .all(|id| id.starts_with("browser_more_"))
        );
        // A window of 64K or less: every browser tool waits, as before.
        assert_eq!(
            names(&select(SMALL_WINDOW).advertised),
            ["read", LOAD_TOOL_NAME]
        );
        // A deferred browser tool the model called by name stays loaded
        // after a restart; one always at hand needs no record.
        assert!(!is_essential_tool("browser_more_00") && is_essential_tool("browser_click"));
        let tools = definitions
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        let items = [
            ran("1", "browser_more_07", false, Value::Null),
            ran("2", "browser_click", false, Value::Null),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        assert_eq!(
            promoted_in(&items, &tools),
            HashSet::from(["browser_more_07".to_owned()])
        );
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
        // A tight window, where the skill tool waits behind the loader even
        // with skills to list.
        let definitions = vec![
            definition(
                "skill",
                &format!(
                    "Load a specialized Agent Skill.{}",
                    skill_list(&[("release-check", "Check a release is ready")])
                ),
            ),
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
            Some(TIGHT_WINDOW),
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

    fn skill_tool(skills: &[(&str, &str)]) -> ToolDefinition {
        definition(
            SKILL_TOOL_NAME,
            &format!("Load a specialized Agent Skill.{}", skill_list(skills)),
        )
    }

    /// Lazy loading on, as with an MCP server: the skill tool and its list
    /// go with every request while there are skills, so the model knows
    /// them without a load; with none, or in a tight window, it waits.
    #[test]
    fn the_skill_tool_stays_at_hand_while_it_lists_skills() {
        let mut definitions: Vec<_> = (0..12)
            .map(|index| {
                definition(
                    &format!("mcp__docs__tool_{index}"),
                    &"a long description of a documentation tool ".repeat(40),
                )
            })
            .collect();
        definitions.push(definition("read", "read a file"));
        definitions.push(definition("skill_manage", "Create or edit Agent Skills"));
        let with = |skill: ToolDefinition| {
            let mut tools = definitions.clone();
            tools.push(skill);
            tools
        };
        let select = |tools: &[ToolDefinition], mode, window, promoted: &[&str]| {
            select_tools(
                tools.to_vec(),
                &HashMap::new(),
                &HashMap::new(),
                &LazyToolSettings {
                    mode,
                    ..LazyToolSettings::default()
                },
                &promoted
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect::<HashSet<_>>(),
                Some(window),
                false,
            )
        };
        let listed = with(skill_tool(&[
            ("release-check", "Check a release is ready"),
            ("review", "Review a change"),
        ]));
        for mode in [LazyToolMode::Auto, LazyToolMode::Always] {
            let selection = select(&listed, mode, 200_000, &[]);
            assert!(selection.response.lazy_active, "{mode:?}");
            assert_eq!(
                names(&selection.advertised),
                ["read", SKILL_TOOL_NAME, LOAD_TOOL_NAME],
                "{mode:?}"
            );
            let entry = selection
                .response
                .tools
                .iter()
                .find(|entry| entry.id == SKILL_TOOL_NAME)
                .unwrap();
            assert!(entry.essential && entry.advertised, "{entry:?}");
            assert_eq!(entry.group, "extensions");
            // The loader says the skill tool is there already, and loads
            // the manager whose id extends its name.
            for query in ["skill", "Skill"] {
                assert_eq!(
                    search_deferred_tools(&listed, &selection.response, &[query.into()]),
                    ["skill_manage"]
                );
                assert_eq!(
                    already_available(&listed, &selection.response, &[query.into()]),
                    [SKILL_TOOL_NAME]
                );
            }
            assert_eq!(
                search_deferred_tools(&listed, &selection.response, &["skills".into()]),
                ["skill_manage"]
            );
        }
        // Without lazy loading the list is sent as before.
        let off = select(&listed, LazyToolMode::Off, 200_000, &[]);
        assert_eq!(off.advertised.len(), listed.len());

        // No skills (or only ones a person starts): nothing to tell the
        // model, so the tool waits behind the loader as it did.
        for empty in [
            definition(SKILL_TOOL_NAME, "Load a specialized Agent Skill."),
            skill_tool(&[]),
        ] {
            let tools = with(empty);
            let selection = select(&tools, LazyToolMode::Auto, 200_000, &[]);
            assert_eq!(names(&selection.advertised), ["read", LOAD_TOOL_NAME]);
            assert_eq!(
                search_deferred_tools(&tools, &selection.response, &["skill".into()]),
                [SKILL_TOOL_NAME, "skill_manage"]
            );
        }

        // A tight window keeps only the project's own tools: the loader
        // finds the skill tool, and once loaded it stays.
        let tight = select(&listed, LazyToolMode::Auto, TIGHT_WINDOW, &[]);
        assert_eq!(names(&tight.advertised), ["read", LOAD_TOOL_NAME]);
        assert_eq!(
            search_deferred_tools(&listed, &tight.response, &["skill".into()]),
            [SKILL_TOOL_NAME, "skill_manage"]
        );
        let loaded = select(
            &listed,
            LazyToolMode::Auto,
            TIGHT_WINDOW,
            &[SKILL_TOOL_NAME],
        );
        assert!(names(&loaded.advertised).contains(&SKILL_TOOL_NAME));
    }

    #[test]
    fn a_short_skill_list_is_written_as_it_always_was() {
        assert_eq!(
            skill_list(&[
                ("release-check", "Check a release is ready"),
                ("review", "Review a change"),
            ]),
            " Available skills: release-check (Check a release is ready). review (Review a change)."
        );
        assert_eq!(skill_list(&[]), " Available skills:");
        // Long descriptions stay whole while the list fits.
        let long = "word ".repeat(300);
        let list = skill_list(&[("one", long.as_str())]);
        assert!(list.len() < SKILL_LIST_BUDGET);
        assert_eq!(list, format!(" Available skills: one ({long})."));
    }

    #[test]
    fn a_long_skill_list_stays_within_its_budget() {
        let described = |list: &str| {
            list.strip_prefix(SKILL_LIST_OPENING)
                .unwrap()
                .split(SKILL_NAMES_OPENING)
                .next()
                .unwrap()
                .to_owned()
        };
        let listed_by_name = |list: &str| {
            list.split_once(SKILL_NAMES_OPENING)
                .map(|(_, names)| {
                    names
                        .trim()
                        .trim_end_matches('.')
                        .split(", ")
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };

        // Many skills with long descriptions.
        let names = (0..120)
            .map(|index| format!("skill-{index:03}"))
            .collect::<Vec<_>>();
        let description = format!("Does one careful thing. {}", "More detail. ".repeat(60));
        let skills = names
            .iter()
            .map(|name| (name.as_str(), description.as_str()))
            .collect::<Vec<_>>();
        let list = skill_list(&skills);
        assert!(list.len() <= SKILL_LIST_BUDGET, "{}", list.len());
        let shown = described(&list);
        let rest = listed_by_name(&list);
        assert!(
            !rest.is_empty() && rest.len() < names.len(),
            "{}",
            rest.len()
        );
        // The first skills are described, in order, each description cut
        // to the limit; the others follow by name, in order.
        let count = names.len() - rest.len();
        for (index, name) in names.iter().enumerate() {
            let entry = format!(" {name} (");
            assert_eq!(shown.contains(&entry), index < count, "{name}");
        }
        assert_eq!(rest, names[count..]);
        let first = shown
            .strip_prefix(" skill-000 (")
            .unwrap()
            .split(").")
            .next()
            .unwrap();
        assert_eq!(first.chars().count(), SKILL_DESCRIPTION_LIMIT);
        assert!(first.ends_with('…') && first.starts_with("Does one careful thing."));

        // One skill whose description alone passes the budget: cut, and
        // nothing named.
        let huge = "x".repeat(SKILL_LIST_BUDGET * 2);
        let list = skill_list(&[("big", huge.as_str()), ("small", "Small")]);
        assert!(listed_by_name(&list).is_empty());
        assert!(list.ends_with(" small (Small)."), "{list}");
        assert!(list.len() < 400);

        // Text of several bytes a character is cut between characters.
        let wide = "日本語の説明".repeat(600);
        let list = skill_list(&[("wide", wide.as_str())]);
        let kept = list
            .strip_prefix(" Available skills: wide (")
            .unwrap()
            .strip_suffix(").")
            .unwrap();
        assert_eq!(kept.chars().count(), SKILL_DESCRIPTION_LIMIT);

        // More names than half the budget holds: the first are described,
        // the next named, and the rest counted, all within the budget, with
        // the way to reach the counted ones.
        let many = (0..1_000)
            .map(|index| format!("plugin-name:skill-{index:04}"))
            .collect::<Vec<_>>();
        let skills = many
            .iter()
            .map(|name| (name.as_str(), "A skill"))
            .collect::<Vec<_>>();
        let list = skill_list(&skills);
        assert!(list.len() <= SKILL_LIST_BUDGET, "{}", list.len());
        let (_, section) = list.split_once(SKILL_NAMES_OPENING).unwrap();
        let section = section.strip_suffix(SKILL_NAMES_ROUTE).unwrap();
        assert!(
            section.len() <= SKILL_LIST_BUDGET / 2 + 2,
            "{}",
            section.len()
        );
        let shown = described(&list).matches(" (A skill).").count();
        let (listed, counted) = section.trim_end_matches('.').rsplit_once(" and ").unwrap();
        let listed = listed.trim().split(", ").collect::<Vec<_>>();
        assert!(shown > 0 && listed.len() > 100, "{shown} {}", listed.len());
        assert_eq!(listed, many[shown..shown + listed.len()]);
        assert_eq!(
            counted,
            format!("{} more", many.len() - shown - listed.len())
        );
    }

    /// Past the budget, the person's own skills are described ahead of the
    /// plugins' that sort before them; under it, the list is as given.
    #[test]
    fn a_long_skill_list_describes_the_persons_own_skills_first() {
        let long = "Explains a plugin procedure at length. ".repeat(8);
        let mut ids = (0..60)
            .map(|index| format!("aaa-plugin:helper-{index:02}"))
            .collect::<Vec<_>>();
        ids.extend(["release-check", "zz-review"].map(String::from));
        let mut skills = ids
            .iter()
            .map(|name| (name.as_str(), long.as_str()))
            .collect::<Vec<_>>();
        skills[60].1 = "Check a release is ready";
        skills[61].1 = "Review a change";
        let list = skill_list(&skills);
        assert!(list.len() <= SKILL_LIST_BUDGET);
        assert!(
            list.starts_with(
                " Available skills: release-check (Check a release is ready). zz-review (Review a change). aaa-plugin:helper-00 ("
            ),
            "{}",
            &list[..200]
        );
        let (_, section) = list.split_once(SKILL_NAMES_OPENING).unwrap();
        assert!(section.contains("aaa-plugin:helper-59") && !section.contains("release-check"));
        // Every name fits: nothing counted, so no word on reaching them.
        assert!(section.ends_with("aaa-plugin:helper-59."), "{section}");

        let short = [("aaa-plugin:helper", "Help"), ("release-check", "Check")];
        assert_eq!(
            skill_list(&short),
            " Available skills: aaa-plugin:helper (Help). release-check (Check)."
        );
    }

    #[test]
    fn some_names_counts_the_names_past_the_room() {
        let names = ["alpha", "beta", "gamma", "delta"];
        assert_eq!(some_names(&names, 100), "alpha, beta, gamma, delta");
        assert_eq!(some_names(&names, 25), "alpha, beta, gamma, delta");
        assert_eq!(some_names(&names, 24), "alpha, beta and 2 more");
        assert_eq!(some_names(&names, 21), "alpha and 3 more");
        assert_eq!(some_names(&names, 5), "4 more");
        assert_eq!(some_names(&[], 5), "");
        for room in 0..40 {
            let text = some_names(&names, room);
            assert!(text.len() <= room.max("4 more".len()), "{room}: {text}");
        }
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
            // Deferred, and it mentions bash: asked for `bash`, the loader
            // still answers with the tool the model has.
            definition("cloud_workspace", "Remote workspaces run bash themselves"),
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

        let only = load_tool_result(&[], &["bash".to_owned()], &[], &[], catalog);
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

    /// Asked for a tool it has, the model is told so, and gets the
    /// deferred tools whose ids begin or end with that id, but not an MCP
    /// tool holding it inside its id, nor every tool whose description
    /// mentions it.
    #[test]
    fn an_offered_id_loads_the_deferred_tools_named_after_it() {
        let definitions = vec![
            definition("bash", "run a command"),
            definition("bash_output", "read a background command's output"),
            definition("bash_stop", "stop a background command"),
            definition("edit", "change a file"),
            definition("multiedit", "change a file in several places"),
            definition("list", "list a folder"),
            definition("memory_list", "list durable notes"),
            definition("mcp__github__list_issues", "list a repository's issues"),
            definition("read", "read a file"),
            definition(
                "cloud_workspace",
                "Remote workspaces run bash, edit files and list folders",
            ),
        ];
        // A tight window, where the companions of the project's own tools
        // wait behind the loader.
        let tight = select_tools(
            definitions.clone(),
            &HashMap::new(),
            &HashMap::new(),
            &LazyToolSettings {
                mode: LazyToolMode::Always,
                ..LazyToolSettings::default()
            },
            &HashSet::new(),
            Some(TIGHT_WINDOW),
            false,
        );
        assert!(tight.response.lazy_active);
        let queries = ["bash", "EDIT", "list"].map(String::from);
        let loaded = search_deferred_tools(&definitions, &tight.response, &queries);
        let already = already_available(&definitions, &tight.response, &queries);
        assert_eq!(
            loaded,
            ["bash_output", "bash_stop", "multiedit", "memory_list"]
        );
        assert_eq!(already, ["bash", "edit", "list"]);
        let result = load_tool_result(&loaded, &already, &[], &[], &tight.response);
        assert_eq!(result["available"], json!(["bash", "edit", "list"]));
        let message = result["message"].as_str().unwrap();
        assert!(
            message.contains("available on the next model round")
                && message.contains("Already in your tool list"),
            "{message}"
        );
        // A word that is no tool's id still searches ids and descriptions.
        assert_eq!(
            search_deferred_tools(&definitions, &tight.response, &["issues".into()]),
            ["mcp__github__list_issues"]
        );
        assert_eq!(
            search_deferred_tools(&definitions, &tight.response, &["workspaces".into()]),
            ["cloud_workspace"]
        );
    }

    /// The skill tool goes with every request while it lists skills: asked
    /// for it, the loader says so and brings the manager named after it.
    #[test]
    fn asked_for_the_skill_tool_the_loader_brings_its_manager() {
        let definitions = vec![
            definition("read", "read a file"),
            skill_tool(&[("release-check", "Check a release is ready")]),
            definition("skill_manage", "Create or edit Agent Skills"),
            definition("cloud_workspace", "Remote workspaces hold skills"),
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
            Some(200_000),
            false,
        );
        let queries = ["skill".to_owned()];
        let loaded = search_deferred_tools(&definitions, &selection.response, &queries);
        let already = already_available(&definitions, &selection.response, &queries);
        assert_eq!(loaded, ["skill_manage"]);
        assert_eq!(already, [SKILL_TOOL_NAME]);
        let result = load_tool_result(&loaded, &already, &[], &[], &selection.response);
        assert_eq!(result["loaded"][0]["id"], "skill_manage");
        assert_eq!(result["available"], json!([SKILL_TOOL_NAME]));
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
        let value = load_tool_result(
            &[],
            &[],
            &["memory_read".to_owned()],
            &[],
            &selection.response,
        );
        assert_eq!(value["notLoaded"], json!(["memory_read"]));
        let message = value["message"].as_str().unwrap();
        assert!(
            message.contains(&MAX_PROMOTED_TOOLS.to_string()),
            "{message}"
        );
        assert!(!message.contains("No deferred tools matched"), "{message}");

        // What a miss always said, unchanged.
        assert_eq!(
            load_tool_result(&[], &[], &[], &[], &selection.response),
            json!({
                "loaded": [],
                "message": "No deferred tools matched. Try a functional term such as sessions, memory, extensions, automation, or an exact/wildcard tool id.",
            })
        );
    }

    fn deferred_docs(count: usize) -> (Vec<ToolDefinition>, ToolSelection) {
        let mut definitions = (1..=count)
            .map(|index| definition(&format!("mcp__docs__t{index:02}"), "a documentation tool"))
            .collect::<Vec<_>>();
        definitions.push(definition("memory_read", "read durable notes"));
        definitions.push(definition("read", "read a file"));
        let selection = select_tools(
            definitions.clone(),
            &HashMap::new(),
            &HashMap::new(),
            &LazyToolSettings {
                mode: LazyToolMode::Always,
                ..LazyToolSettings::default()
            },
            &HashSet::new(),
            Some(200_000),
            false,
        );
        (definitions, selection)
    }

    /// A tool named by its exact id loads even when a wildcard in the same
    /// call matches more than one call loads, whichever query comes first.
    #[test]
    fn an_exact_id_loads_ahead_of_a_wildcard_in_the_same_call() {
        let (definitions, selection) = deferred_docs(25);
        let docs = |range: std::ops::RangeInclusive<usize>| {
            range
                .map(|index| format!("mcp__docs__t{index:02}"))
                .collect::<Vec<_>>()
        };
        for queries in [
            ["mcp__docs__*", "memory_read"],
            ["memory_read", "mcp__docs__*"],
        ] {
            let queries = queries.map(String::from);
            let matches = find_deferred_tools(&definitions, &selection.response, &queries);
            assert_eq!(matches.found.len(), MAX_LOAD_MATCHES, "{queries:?}");
            assert_eq!(matches.found[0], "memory_read", "{queries:?}");
            assert_eq!(matches.found[1..], docs(1..=19), "{queries:?}");
            assert_eq!(matches.left_out, docs(20..=25), "{queries:?}");
        }
        // An exact id in other letters is still that tool.
        let matches = find_deferred_tools(
            &definitions,
            &selection.response,
            &["mcp__docs__*".into(), "MEMORY_READ".into()],
        );
        assert_eq!(matches.found[0], "memory_read");
    }

    /// One wildcard over more tools than a call loads: the first twenty
    /// load, and the answer names the rest and how to load them.
    #[test]
    fn a_load_past_twenty_names_what_it_left_out() {
        let (definitions, selection) = deferred_docs(25);
        let matches =
            find_deferred_tools(&definitions, &selection.response, &["mcp__docs__*".into()]);
        assert_eq!(matches.found.len(), MAX_LOAD_MATCHES);
        assert_eq!(
            matches.left_out,
            ["t21", "t22", "t23", "t24", "t25"].map(|tool| format!("mcp__docs__{tool}"))
        );
        let value = load_tool_result(
            &matches.found,
            &[],
            &[],
            &matches.left_out,
            &selection.response,
        );
        assert_eq!(value["loaded"].as_array().unwrap().len(), MAX_LOAD_MATCHES);
        let message = value["message"].as_str().unwrap();
        assert!(
            message.contains(
                "past the 20 tools one call loads, so not loaded: mcp__docs__t21, mcp__docs__t22, mcp__docs__t23, mcp__docs__t24, mcp__docs__t25."
            ) && message.ends_with("Load the ones you need by exact id."),
            "{message}"
        );
        // Under the limit, nothing more is said.
        let (definitions, selection) = deferred_docs(5);
        let matches =
            find_deferred_tools(&definitions, &selection.response, &["mcp__docs__*".into()]);
        assert!(matches.left_out.is_empty());
        let value = load_tool_result(&matches.found, &[], &[], &[], &selection.response);
        assert_eq!(
            value["message"],
            "The matching tool schemas will be available on the next model round."
        );
    }

    /// Hundreds left out: the answer names what fits its room and counts
    /// the rest.
    #[test]
    fn the_names_left_out_are_bounded() {
        let (definitions, selection) = deferred_docs(99);
        let mut left_out = (0..400)
            .map(|index| format!("mcp__a_server_with_a_long_name__tool_{index:03}"))
            .collect::<Vec<_>>();
        left_out.extend(definitions.iter().map(|tool| tool.name.clone()));
        let value = load_tool_result(&[], &[], &[], &left_out, &selection.response);
        let message = value["message"].as_str().unwrap();
        assert!(message.len() <= LEFT_OUT_ROOM + 200, "{}", message.len());
        assert!(
            message.contains(" more. Load the ones you need"),
            "{message}"
        );
        assert!(message.contains("mcp__a_server_with_a_long_name__tool_000"));
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
