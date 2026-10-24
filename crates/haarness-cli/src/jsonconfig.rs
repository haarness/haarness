//! JSON/JSONC configuration and ecosystem-format compatibility.
//!
//! Haarness reads, in ascending precedence:
//!   0. the base folder's `haarness.json{,c}` (`HAARNESS_BASE_CONFIG_DIR`,
//!      read only, when set)
//!   1. global `haarness.json{,c}` (including legacy aliases) (config dir)
//!   2. project `haarness.json{,c}` (including legacy aliases) (workspace root)
//!   3. project `.haarness/config.toml` (legacy, trust-gated — handled by
//!      the TOML loader)
//!
//! Native and compatibility resource roots provide commands, agents,
//! and skills. `AGENTS.md` supplies instruction files with `@import`s, and
//! the `instructions` key adds globs, `~/` paths, absolute paths and
//! `http(s)://` URLs to that set. Claude Code home and project files are
//! never discovered unless `compatibility.claude_code.{instructions,agents,
//! skills,commands}` opts in.
//!
//! Supported keys include (`model`, `small_model`,
//! `default_agent`, `permission`, `mcp`, `agent`, `instructions`, `skills`,
//! `compatibility`) including the legacy JSON format. A
//! legacy `config.toml` is migrated to `haarness.json` once.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use haarness_core::instructions::{self as instruction_files, ImportBudget};

use anyhow::Result;
use haarness_core::{CustomAgent, PermissionDecision, PermissionRule};
use haarness_protocol::{CommandInfo, McpConfigureRequest};
use serde_json::Value;

/// Everything the JSON/compat layers contribute on top of the TOML config.
#[derive(Default)]
pub struct JsonLayers {
    pub native: super::FileConfig,
    /// `provider`, `model` etc. defaults (later layers win).
    pub provider: Option<String>,
    pub model: Option<String>,
    pub small_model: Option<String>,
    pub default_agent: Option<String>,
    /// Autonomous goal defaults.
    pub goal_token_budget: Option<u64>,
    pub goal_max_turns: Option<u32>,
    /// Ordered permission rules, already compiled.
    pub rules: Vec<PermissionRule>,
    /// MCP servers by name (trust-gated ones only included when trusted).
    pub mcp: BTreeMap<String, McpConfigureRequest>,
    /// The servers of `mcp` whose entry in effect is the base folder's.
    pub base_mcp: BTreeSet<String>,
    pub commands: Vec<CommandInfo>,
    pub agents: Vec<CustomAgent>,
    /// Lifecycle hook entries from every config layer.
    pub hooks: Vec<haarness_server::HookEntry>,
    /// Source documents for desktop management, including disabled/untrusted hooks.
    pub hook_sources: Vec<HookSource>,
    /// Assembled project instructions (AGENTS.md family + imports).
    pub instructions: Option<String>,
    /// Configured `instructions` entries (globs, `~/` paths, absolute paths,
    /// URLs), in layer order without duplicates.
    pub instruction_sources: Vec<String>,
    /// Every local instruction file the assembly considered, so hot reload
    /// can watch files named by `instructions` entries.
    pub instruction_files: Vec<PathBuf>,
    /// Agent names some layer marked `disable: true` (the
    /// flag survives later layers unless one sets `disable: false`).
    pub disabled_agents: Vec<String>,
    /// Folders scanned for agent markdown, so hot reload can tell an edit
    /// that changes only agent definitions from one that changes more.
    pub agent_directories: Vec<PathBuf>,
    /// Human-readable notes (migrations, skipped files) for logs.
    pub notices: Vec<String>,
    /// Existing JSON/JSONC inputs that could not be read or parsed. Startup
    /// may report and continue for compatibility; hot reload rejects the
    /// whole candidate and retains the last-known-good generation.
    pub errors: Vec<String>,
    /// `auto_review` from every layer: the base and global layers set it,
    /// a project's only add `block` entries (kept apart) and lower `timeout`.
    pub auto_review: haarness_core::AutoReviewSettings,
    /// The project layers are loading.
    pub(crate) project: bool,
}

pub struct HookSource {
    pub path: PathBuf,
    pub block: Value,
    pub trusted: bool,
    pub plugin: bool,
    /// A file of the base folder, which Haarness never changes.
    pub base: bool,
}

/// The JSON and JSONC files a configuration folder holds, in the order they
/// merge.
const CONFIG_JSON: [&str; 4] = [
    "opencode.jsonc",
    "opencode.json",
    "haarness.jsonc",
    "haarness.json",
];

/// Load every JSON/compat layer. `config_dir` is the directory holding the
/// global haarness config; `home` is the user's home directory. The base
/// folder `HAARNESS_BASE_CONFIG_DIR` names, when set, is read below every
/// other layer.
pub fn load_layers(
    config_dir: &Path,
    home: Option<&Path>,
    workspace: &Path,
    trust: bool,
    plugin_roots: &[PathBuf],
) -> JsonLayers {
    let base = crate::layout::BaseFolder::for_workspace(workspace);
    load_layers_with_base(
        config_dir,
        base.as_ref(),
        home,
        workspace,
        trust,
        plugin_roots,
    )
}

/// [`load_layers`] with the base folder given rather than read from the
/// environment.
pub fn load_layers_with_base(
    config_dir: &Path,
    base: Option<&crate::layout::BaseFolder>,
    home: Option<&Path>,
    workspace: &Path,
    trust: bool,
    plugin_roots: &[PathBuf],
) -> JsonLayers {
    let mut layers = JsonLayers::default();

    // The base folder: settings a team or device management ships, which
    // every other layer overrides. Only with one, so loading without it takes
    // exactly the steps it always has.
    if let Some(base) = base {
        if !base.root().is_dir() {
            layers.notices.push(format!(
                "base config folder {} not found",
                base.root().display()
            ));
        }
        for name in CONFIG_JSON {
            if let Some(path) = base.entry(name) {
                apply_json_layer(&mut layers, &path, true, true);
            }
        }
    }
    apply_toml(&mut layers, &config_dir.join("config.toml"));
    // Global JSON configuration.
    for name in [
        "opencode.jsonc",
        "opencode.json",
        "haarness.jsonc",
        "haarness.json",
    ] {
        apply_json_config(&mut layers, &config_dir.join(name), true);
    }
    let global_providers = layers.native.providers.clone();
    layers.project = true;
    // Project files override global defaults; nested worktree layers override ancestors.
    for directory in crate::layout::project_directories(workspace)
        .into_iter()
        .filter(|_| trust)
    {
        // Session defaults are portable project files. Ordinary repository
        // configuration keeps its existing precedence over Cloud defaults.
        apply_json_config(
            &mut layers,
            &directory.join(".haarness/cloud/haarness.json"),
            trust,
        );
        apply_toml(&mut layers, &directory.join(".haarness/config.toml"));
        for root in [
            directory.clone(),
            directory.join(".opencode"),
            directory.join(".haarness"),
        ] {
            for name in [
                "opencode.jsonc",
                "opencode.json",
                "haarness.jsonc",
                "haarness.json",
            ] {
                apply_json_config(&mut layers, &root.join(name), trust);
            }
        }
    }
    layers.project = false;
    layers.native.provider_notices =
        super::guard_project_providers(&global_providers, &mut layers.native.providers);
    // Plugin bundles: the user installed them, so their hooks/MCP are
    // trusted. Commands/agents merge like any other root.
    for root in plugin_roots {
        let hooks_json = root.join("hooks/hooks.json");
        if let Some(value) = read_json_recording(&mut layers, &hooks_json) {
            let block = value.get("hooks").unwrap_or(&value);
            layers.hook_sources.push(HookSource {
                path: hooks_json.clone(),
                block: block.clone(),
                trusted: true,
                plugin: true,
                base: false,
            });
            layers
                .hooks
                .extend(haarness_server::ConfiguredHooks::parse_plugin_block(
                    block,
                    root.clone(),
                ));
        }
        apply_plugin_mcp(&mut layers, root);
        layers.notices.push(format!("plugin {}", root.display()));
    }

    // Resource overrides use the same global -> ancestor -> project ordering.
    let resources = crate::layout::resource_roots(config_dir, home, workspace, trust);
    let compat = super::claude_compat(&layers.native);
    // The base folder's commands, skills and agents come first, so every
    // other root replaces one of the same name.
    if let Some(base) = base {
        discover_base_resources(&mut layers, base);
    }
    // Claude Code roots are compatibility imports: they load before native
    // roots so a Haarness resource with the same name wins.
    if compat.any() {
        for directory in claude_directories(home, workspace, trust) {
            if compat.commands() {
                let commands = directory.join("commands");
                discover_commands(
                    &mut layers.commands,
                    &mut layers.notices,
                    &commands,
                    &commands,
                    None,
                );
            }
            if compat.agents() {
                let agents = directory.join("agents");
                layers.agent_directories.push(agents.clone());
                discover_agents_with(
                    &mut layers.agents,
                    &mut layers.notices,
                    &agents,
                    &agents,
                    None,
                    true,
                );
            }
            if compat.skills() {
                discover_skill_commands(
                    &mut layers.commands,
                    &mut layers.notices,
                    &directory.join("skills"),
                    None,
                );
            }
        }
    }
    for root in plugin_roots {
        let plugin = plugin_name(root);
        for directory in haarness_core::definitions::component_paths(root, "commands") {
            discover_commands(
                &mut layers.commands,
                &mut layers.notices,
                &directory,
                &directory,
                plugin.as_deref(),
            );
        }
        for directory in haarness_core::definitions::component_paths(root, "skills") {
            discover_skill_commands(
                &mut layers.commands,
                &mut layers.notices,
                &directory,
                plugin.as_deref(),
            );
        }
        for directory in haarness_core::definitions::component_paths(root, "agents") {
            layers.agent_directories.push(directory.clone());
            discover_agents(
                &mut layers.agents,
                &mut layers.notices,
                &directory,
                &directory,
                plugin.as_deref(),
            );
        }
    }
    for root in &resources {
        for name in ["command", "commands"] {
            let directory = root.join(name);
            discover_commands(
                &mut layers.commands,
                &mut layers.notices,
                &directory,
                &directory,
                None,
            );
        }
        for name in ["skill", "skills"] {
            discover_skill_commands(
                &mut layers.commands,
                &mut layers.notices,
                &root.join(name),
                None,
            );
        }
        for name in ["agent", "agents"] {
            let directory = root.join(name);
            layers.agent_directories.push(directory.clone());
            discover_agents(
                &mut layers.agents,
                &mut layers.notices,
                &directory,
                &directory,
                None,
            );
        }
    }
    // Agent Skills roots share the same command surface as Haarness skills.
    if let Some(home) = home {
        discover_skill_commands(
            &mut layers.commands,
            &mut layers.notices,
            &home.join(".agents/skills"),
            None,
        );
    }
    for directory in crate::layout::project_directories(workspace)
        .into_iter()
        .filter(|_| trust)
    {
        discover_skill_commands(
            &mut layers.commands,
            &mut layers.notices,
            &directory.join(".agents/skills"),
            None,
        );
    }
    // `skills.paths`: extra skill folders, `~/` or workspace-relative.
    for path in configured_skill_paths(&layers.native, home, workspace) {
        discover_skill_commands(&mut layers.commands, &mut layers.notices, &path, None);
    }

    // Instructions: global files, then project instruction files from the
    // worktree root down, then configured `instructions` entries and URLs.
    let claude = compat.instructions();
    let mut local_files: Vec<(PathBuf, bool)> = Vec::new();
    if let Some(file) = base.and_then(|base| base.entry("AGENTS.md")) {
        local_files.push((file, true));
    }
    local_files.push((config_dir.join("AGENTS.md"), true));
    if claude && let Some(home) = home {
        local_files.push((home.join(".claude/CLAUDE.md"), true));
    }
    for directory in crate::layout::project_directories(workspace)
        .into_iter()
        .filter(|_| trust)
    {
        if claude {
            local_files.extend(
                instruction_files::find_in(&directory, true)
                    .into_iter()
                    .map(|file| (file, false)),
            );
        } else {
            local_files.push((directory.join("AGENTS.md"), false));
        }
    }
    let trusted_roots = trusted_instruction_roots(config_dir, home);
    let mut urls = Vec::new();
    let mut seen: HashSet<PathBuf> = local_files
        .iter()
        .filter_map(|(file, _)| file.canonicalize().ok())
        .collect();
    for entry in &layers.instruction_sources {
        if entry.starts_with("https://") || entry.starts_with("http://") {
            urls.push(entry.clone());
            continue;
        }
        for file in instruction_entry_files(entry, home, workspace, trust) {
            let Ok(canonical) = file.canonicalize() else {
                continue;
            };
            if !seen.insert(canonical.clone()) {
                continue;
            }
            let trusted = trusted_roots.iter().any(|root| canonical.starts_with(root));
            local_files.push((file, trusted));
        }
    }
    layers.instruction_files = local_files.iter().map(|(file, _)| file.clone()).collect();
    let mut segments = Vec::new();
    let mut import_budget = ImportBudget::default();
    // Nothing asks before instructions load, so an import a configured
    // `read` rule denies is left out rather than put in every prompt.
    let mut read_rules = layers
        .native
        .permissions
        .as_ref()
        .and_then(|permissions| super::compile_permissions(permissions).ok())
        .unwrap_or_default();
    read_rules.extend(layers.rules.iter().cloned());
    let mut read_rules = haarness_core::PermissionEngine::new(read_rules);
    read_rules.set_workspace(workspace);
    let denied = |file: &Path| {
        (read_rules.evaluate(&format!("read:{}", file.display()))
            == haarness_core::PermissionDecision::Deny)
            .then_some("a read rule denies it")
    };
    for (file, trusted) in local_files {
        if let Some(content) = instruction_files::read_with_imports(
            &file,
            workspace,
            &mut import_budget,
            trusted,
            &denied,
        ) {
            segments.push(instruction_files::segment(
                &file.display().to_string(),
                &content,
            ));
        }
    }
    for url in urls {
        // The address can carry a key in its user name or query: the prompt
        // and the notices name it with those parts hidden.
        let shown = haarness_core::redact::url(&url);
        match fetch_remote_instructions(&url) {
            Ok(body) if !body.trim().is_empty() => {
                segments.push(instruction_files::segment(&shown, &body));
            }
            Ok(_) => layers
                .notices
                .push(format!("remote instructions {shown} are empty")),
            Err(error) => layers.notices.push(format!(
                "skipped remote instructions {shown}: {}",
                haarness_core::redact::urls_in(&error)
            )),
        }
    }
    let assembled = segments.join("\n\n");
    if !assembled.trim().is_empty() {
        layers.instructions = Some(assembled);
    }
    let disabled = std::mem::take(&mut layers.disabled_agents);
    layers.agents.retain(|agent| !disabled.contains(&agent.id));
    layers.disabled_agents = disabled;
    layers
}

/// The base folder's commands, skills and agents, each file read where it
/// is unless a link leads it into the project.
fn discover_base_resources(layers: &mut JsonLayers, base: &crate::layout::BaseFolder) {
    for name in ["command", "commands"] {
        let directory = base.root().join(name);
        for file in base.files(name) {
            discover_commands(
                &mut layers.commands,
                &mut layers.notices,
                &file,
                &directory,
                None,
            );
        }
    }
    for name in ["skill", "skills"] {
        for file in base.files(name) {
            if file.file_name().is_some_and(|file| file == "SKILL.md") {
                discover_skill_commands(&mut layers.commands, &mut layers.notices, &file, None);
            }
        }
    }
    for name in ["agent", "agents"] {
        let directory = base.root().join(name);
        layers.agent_directories.push(directory.clone());
        for file in base.files(name) {
            discover_agents(
                &mut layers.agents,
                &mut layers.notices,
                &file,
                &directory,
                None,
            );
        }
    }
}

/// The agents and commands the base folder alone defines, in its settings
/// and its folders, so a list of the merged ones can tell which come from it.
pub fn base_definitions(base: &crate::layout::BaseFolder) -> (Vec<CustomAgent>, Vec<CommandInfo>) {
    let mut layers = JsonLayers::default();
    for name in CONFIG_JSON {
        if let Some(path) = base.entry(name) {
            apply_json_layer(&mut layers, &path, true, true);
        }
    }
    discover_base_resources(&mut layers, base);
    (layers.agents, layers.commands)
}

/// `~/.claude` and every project `.claude` directory (worktree root first).
fn claude_directories(home: Option<&Path>, workspace: &Path, trust: bool) -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if let Some(home) = home {
        directories.push(home.join(".claude"));
    }
    for directory in crate::layout::project_directories(workspace)
        .into_iter()
        .filter(|_| trust)
    {
        directories.push(directory.join(".claude"));
    }
    directories
}

/// Expand a configured path: `~/` against the home directory, relative
/// paths against the workspace.
pub(super) fn expand_configured_path(
    entry: &str,
    home: Option<&Path>,
    workspace: &Path,
) -> Option<PathBuf> {
    if let Some(rest) = entry.strip_prefix("~/") {
        return home.map(|home| home.join(rest));
    }
    let path = Path::new(entry);
    Some(if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    })
}

/// Existing `skills.paths` directories of a merged configuration.
pub(super) fn configured_skill_paths(
    config: &super::FileConfig,
    home: Option<&Path>,
    workspace: &Path,
) -> Vec<PathBuf> {
    config
        .skills
        .as_ref()
        .map(|skills| skills.paths.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| expand_configured_path(entry, home, workspace))
        .filter(|path| path.is_dir())
        .collect()
}

/// Roots whose instruction files may `@import` from outside the workspace.
fn trusted_instruction_roots(config_dir: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = vec![config_dir.to_path_buf()];
    if let Some(home) = home {
        roots.extend(
            [".haarness", ".opencode", ".claude"]
                .iter()
                .map(|name| home.join(name)),
        );
    }
    roots
        .into_iter()
        .filter_map(|root| root.canonicalize().ok())
        .collect()
}

/// Files named by one `instructions` entry: `~/` and absolute entries are
/// globbed in place; relative entries glob upward from the workspace to the
/// worktree root and are skipped when project
/// configuration is disabled.
fn instruction_entry_files(
    entry: &str,
    home: Option<&Path>,
    workspace: &Path,
    trust: bool,
) -> Vec<PathBuf> {
    let files = |pattern: String| {
        glob::glob(&pattern)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|path| path.is_file())
            .collect::<Vec<_>>()
    };
    let entry = match entry.strip_prefix("~/") {
        Some(rest) => {
            let Some(home) = home else {
                return Vec::new();
            };
            format!("{}/{rest}", glob::Pattern::escape(&home.to_string_lossy()))
        }
        None => entry.to_owned(),
    };
    if Path::new(&entry).is_absolute() {
        return files(entry);
    }
    if !trust {
        return Vec::new();
    }
    let mut out = Vec::new();
    for directory in crate::layout::project_directories(workspace)
        .into_iter()
        .rev()
    {
        out.extend(files(format!(
            "{}/{entry}",
            glob::Pattern::escape(&directory.to_string_lossy())
        )));
    }
    out
}

/// Largest remote instruction body that is inlined.
const REMOTE_INSTRUCTION_BYTES: usize = 200 * 1024;

/// Fetch a remote instruction file (10 s, 200 KiB, UTF-8). Successful bodies
/// are kept for the process lifetime so hot reloads do not refetch.
fn fetch_remote_instructions(url: &str) -> Result<String, String> {
    let bytes = fetch_url(url, REMOTE_INSTRUCTION_BYTES)?;
    String::from_utf8(bytes).map_err(|_| "body is not UTF-8".to_owned())
}

/// Blocking GET on a dedicated thread with its own runtime, so it is safe
/// from both synchronous and asynchronous callers. Bodies larger than `cap`
/// are rejected; successes are memoized per URL for the process lifetime.
pub(super) fn fetch_url(url: &str, cap: usize) -> Result<Vec<u8>, String> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, Vec<u8>>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().ok().and_then(|cache| cache.get(url).cloned()) {
        return Ok(hit);
    }
    let target = url.to_owned();
    let fetched = std::thread::spawn(move || -> Result<Vec<u8>, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        runtime.block_on(async move {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .map_err(|error| error.to_string())?;
            let mut response = client
                .get(&target)
                .send()
                .await
                .map_err(|error| error.to_string())?;
            if !response.status().is_success() {
                return Err(format!("HTTP {}", response.status()));
            }
            if response
                .content_length()
                .is_some_and(|length| length > cap as u64)
            {
                return Err(format!("body exceeds {cap} bytes"));
            }
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
                body.extend_from_slice(&chunk);
                if body.len() > cap {
                    return Err(format!("body exceeds {cap} bytes"));
                }
            }
            Ok(body)
        })
    })
    .join()
    .map_err(|_| "fetch thread panicked".to_owned())??;
    if let Ok(mut cache) = cache.lock() {
        cache.insert(url.to_owned(), fetched.clone());
    }
    Ok(fetched)
}

/// Largest remote skill index or file that is cached.
const REMOTE_SKILL_BYTES: usize = 1024 * 1024;

/// Pull one `skills.urls` index into `cache/<host>/<skill>/…` and return
/// that host root when at least one `SKILL.md` is available. The index is
/// `{"skills":[{"name","description","files":["SKILL.md",…]}]}` with files
/// relative to `<base>/<name>/`, or an entry may name
/// its skill file directly with `"url"`. Cached files are never refetched.
pub(super) fn pull_remote_skills(cache: &Path, url: &str) -> Result<Option<PathBuf>, String> {
    let url = url.trim();
    let (index, base) = match url.strip_suffix("index.json") {
        Some(base) => (url.to_owned(), base.to_owned()),
        None => {
            let base = if url.ends_with('/') {
                url.to_owned()
            } else {
                format!("{url}/")
            };
            (format!("{base}index.json"), base)
        }
    };
    let body = fetch_url(&index, REMOTE_SKILL_BYTES)?;
    let value: Value =
        serde_json::from_slice(&body).map_err(|error| format!("invalid index: {error}"))?;
    let skills = value
        .get("skills")
        .and_then(Value::as_array)
        .ok_or_else(|| "index has no skills array".to_owned())?;
    let host = cache.join(remote_cache_key(&base));
    let mut usable = false;
    for skill in skills {
        let Some(name) = skill
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| valid_path_component(name))
        else {
            continue;
        };
        // A listed file is fetched from the skill's own folder on the index
        // host; only an entry's `url` may name another address.
        let folder = format!("{base}{name}/");
        let files: Vec<(String, String)> = match skill.get("url").and_then(Value::as_str) {
            Some(direct) => vec![("SKILL.md".to_owned(), resolve_link(&base, direct))],
            None => skill
                .get("files")
                .and_then(Value::as_array)
                .map(|files| {
                    files
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| vec!["SKILL.md".to_owned()])
                .into_iter()
                .map(|file| {
                    let link = format!("{folder}{file}");
                    (file, link)
                })
                // `%2e%2e/` stays one folder on disk, but the address parser
                // reads it as `..`.
                .filter(|(file, link)| {
                    let inside = stays_under(link, &folder);
                    if !inside {
                        tracing::warn!("config: skipped remote skill file {file}: its address leaves the skill's folder");
                    }
                    inside
                })
                .collect(),
        };
        let root = host.join(name);
        for (file, link) in files {
            let relative = Path::new(&file);
            if relative.is_absolute()
                || !relative
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_)))
            {
                continue;
            }
            let destination = root.join(relative);
            if destination.exists() {
                continue;
            }
            match fetch_url(&link, REMOTE_SKILL_BYTES) {
                Ok(bytes) => {
                    if let Some(parent) = destination.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    if let Err(error) = fs::write(&destination, bytes) {
                        tracing::warn!("config: cannot cache remote skill file {link}: {error}");
                    }
                }
                Err(error) => tracing::warn!("config: skipped remote skill file {link}: {error}"),
            }
        }
        usable |= root.join("SKILL.md").is_file();
    }
    Ok(usable.then_some(host))
}

/// Resolve an index link: absolute URLs pass through, `/path` joins the
/// index origin, anything else joins the index base.
fn resolve_link(base: &str, link: &str) -> String {
    if link.starts_with("http://") || link.starts_with("https://") {
        return link.to_owned();
    }
    if let Some(rest) = link.strip_prefix('/') {
        let origin = base
            .find("://")
            .map(|scheme| scheme + 3)
            .and_then(|start| base[start..].find('/').map(|end| &base[..start + end]))
            .unwrap_or(base.trim_end_matches('/'));
        return format!("{origin}/{rest}");
    }
    format!("{base}{link}")
}

/// Whether `link`, read as an address, is still inside the `folder` address.
fn stays_under(link: &str, folder: &str) -> bool {
    match (reqwest::Url::parse(link), reqwest::Url::parse(folder)) {
        (Ok(link), Ok(folder)) => link.as_str().starts_with(folder.as_str()),
        _ => false,
    }
}

fn remote_cache_key(base: &str) -> String {
    let stripped = base
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/');
    let key: String = stripped
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .take(100)
        .collect();
    let key = key.trim_matches('-').to_owned();
    if key.is_empty() { "remote".into() } else { key }
}

fn valid_path_component(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn apply_toml(layers: &mut JsonLayers, path: &Path) {
    if !path.exists() {
        return;
    }
    let result = (|| -> anyhow::Result<()> {
        let mut config: super::FileConfig = toml::from_str(&fs::read_to_string(path)?)?;
        merge_auto_review(layers, config.auto_review.take(), path);
        if let Some(small) = config
            .defaults
            .as_ref()
            .and_then(|defaults| defaults.small_model.clone())
            .filter(|_| !layers.project)
        {
            layers.auto_review.small_model = Some(small);
        }
        if let Some(permissions) = &config.permissions {
            layers
                .rules
                .extend(super::compile_permissions(permissions)?);
        }
        for name in config.mcp.keys() {
            layers.base_mcp.remove(name);
        }
        layers.mcp.extend(config.mcp.clone());
        super::merge_file_config(&mut layers.native, config);
        Ok(())
    })();
    if let Err(error) = result {
        layers
            .errors
            .push(format!("invalid configuration {}: {error}", path.display()));
    }
}

/// One layer's `auto_review` onto what the layers below it set. A project's
/// files come from the repository, so they may only make the review
/// stricter: add `block` entries, which the reviewer reads as untrusted, and
/// lower `timeout`. Anything else they set is left out, with a notice.
fn merge_auto_review(
    layers: &mut JsonLayers,
    section: Option<super::AutoReviewSection>,
    path: &Path,
) {
    let Some(section) = section else {
        return;
    };
    let review = &mut layers.auto_review;
    let timeout = section
        .timeout
        .map(|seconds| Duration::from_secs(seconds.max(1)));
    if layers.project {
        let mut ignored = Vec::new();
        if section.enabled.is_some() {
            ignored.push("enabled");
        }
        if section.model.is_some() {
            ignored.push("model");
        }
        if !section.environment.is_empty() {
            ignored.push("environment");
        }
        if !section.allow.is_empty() {
            ignored.push("allow");
        }
        match timeout {
            Some(timeout) if timeout <= review.timeout => review.timeout = timeout,
            Some(_) => ignored.push("timeout"),
            None => {}
        }
        review.project_block.extend(section.block);
        if !ignored.is_empty() {
            layers.notices.push(format!(
                "{}: ignored auto_review.{}: a project may only add block entries and lower the timeout",
                path.display(),
                ignored.join(", auto_review.")
            ));
        }
        return;
    }
    if let Some(enabled) = section.enabled {
        review.enabled = enabled;
    }
    if let Some(model) = section.model {
        if model
            .split_once('/')
            .is_some_and(|(provider, model)| !provider.is_empty() && !model.is_empty())
        {
            review.model = Some(model);
        } else {
            layers.notices.push(format!(
                "{}: ignored auto_review.model {model:?}: write it as provider/model",
                path.display()
            ));
        }
    }
    review.environment.extend(section.environment);
    review.allow.extend(section.allow);
    review.block.extend(section.block);
    if let Some(timeout) = timeout {
        review.timeout = timeout;
    }
}

/// Read a JSON or JSONC file into a `serde_json::Value` (order preserved).
pub(super) fn read_json(path: &Path) -> Result<Option<Value>, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    jsonc_parser::parse_to_serde_value(&text, &jsonc_parser::ParseOptions::default())
        .map_err(|error| format!("invalid JSON in {}: {error}", path.display()))
}

fn read_json_recording(layers: &mut JsonLayers, path: &Path) -> Option<Value> {
    match read_json(path) {
        Ok(value) => value,
        Err(error) => {
            layers.errors.push(error);
            None
        }
    }
}

/// JSON config: `model`, `small_model`, `default_agent`,
/// `permission`, `mcp`, `agent`, `command`, `instructions`.
fn apply_json_config(layers: &mut JsonLayers, path: &Path, trusted: bool) {
    apply_json_layer(layers, path, trusted, false);
}

/// [`apply_json_config`] for a file of any layer. A `base` file (one of the
/// base folder's) is read as a global one is, except that its hooks are
/// marked read-only and its extension and plugin declarations, which only
/// the global and project layers make, are not reported.
fn apply_json_layer(layers: &mut JsonLayers, path: &Path, trusted: bool, base: bool) {
    let Some(value) = read_json_recording(layers, path) else {
        return;
    };
    layers.notices.push(format!("loaded {}", path.display()));
    // Shared native fields (providers, tools, compaction, language services,
    // snapshots) obey the same scopes as model, permission and MCP settings.
    if trusted {
        let mut native = value.clone();
        if let Some(object) = native.as_object_mut() {
            object.remove("mcp");
        }
        match serde_json::from_value::<super::FileConfig>(native) {
            Ok(mut config) => {
                merge_auto_review(layers, config.auto_review.take(), path);
                super::merge_file_config(&mut layers.native, config);
            }
            Err(error) => layers
                .errors
                .push(format!("invalid configuration {}: {error}", path.display())),
        }
    }
    if let Some(model) = value.get("model").and_then(Value::as_str) {
        if let Some((provider, model)) = model.split_once('/') {
            layers.provider = Some(provider.to_owned());
            layers.model = Some(model.to_owned());
            let defaults = layers
                .native
                .defaults
                .get_or_insert_with(super::DefaultsSection::default);
            defaults.provider = Some(provider.to_owned());
            defaults.model = Some(model.to_owned());
        }
    }
    if let Some(small) = value.get("small_model").and_then(Value::as_str) {
        layers.small_model = Some(small.to_owned());
        // Auto's reviewer reads the person's messages: a project's file
        // never picks where they go.
        if !layers.project {
            layers.auto_review.small_model = Some(small.to_owned());
        }
    }
    if let Some(agent) = value.get("default_agent").and_then(Value::as_str) {
        layers.default_agent = Some(agent.to_owned());
    }
    if let Some(goal) = value
        .get("experimental")
        .and_then(|value| value.get("goal_harness"))
    {
        if let Some(token_budget) = goal
            .get("tokenBudget")
            .and_then(Value::as_u64)
            .filter(|value| *value > 0)
        {
            layers.goal_token_budget = Some(token_budget);
        }
        if let Some(max_turns) = goal
            .get("maxTurns")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0)
        {
            layers.goal_max_turns = Some(max_turns);
        }
    }
    if let Some(model) = value.get("small_model").and_then(Value::as_str) {
        layers
            .native
            .defaults
            .get_or_insert_with(super::DefaultsSection::default)
            .small_model = Some(model.into());
    }
    if let Some(agent) = value.get("default_agent").and_then(Value::as_str) {
        layers
            .native
            .defaults
            .get_or_insert_with(super::DefaultsSection::default)
            .agent = Some(agent.into());
    }
    if let Some(goal) = value.pointer("/experimental/goalHarness") {
        let current = JsonLayers {
            goal_token_budget: goal.get("tokenBudget").and_then(Value::as_u64),
            goal_max_turns: goal
                .get("maxTurns")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok()),
            ..JsonLayers::default()
        };
        super::apply_json_goal_config(&mut layers.native, &current);
    }
    if let Some(permission) = value.get("permission") {
        compile_permission(&mut layers.rules, permission, path, "permission");
    }
    if let Some(hooks) = value.get("hooks") {
        layers.hook_sources.push(HookSource {
            path: path.to_path_buf(),
            block: hooks.clone(),
            trusted,
            plugin: false,
            base,
        });
        if trusted {
            layers
                .hooks
                .extend(haarness_server::ConfiguredHooks::parse_block(hooks));
        }
    }
    // Haarness `plugin`, `plugins` and `extensions`: registration
    // happens in `extensions::sources` and the plugin registry
    // (`config_specs`); the loader only reports entries it cannot map.
    if !base {
        layers
            .notices
            .extend(crate::config_specs::describe(&value, path, trusted));
    }
    if trusted {
        if let Some(mcp) = value.get("mcp").and_then(Value::as_object) {
            for (name, entry) in mcp {
                if let Some(request) = loaded_mcp_entry(entry, None) {
                    if base {
                        layers.base_mcp.insert(name.clone());
                    } else {
                        layers.base_mcp.remove(name);
                    }
                    layers.mcp.insert(name.clone(), request);
                }
            }
        }
    }
    if let Some(entries) = value.get("instructions").and_then(Value::as_array) {
        for entry in entries.iter().filter_map(Value::as_str) {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            if !layers
                .instruction_sources
                .iter()
                .any(|known| known == entry)
            {
                layers.instruction_sources.push(entry.to_owned());
            }
        }
    }
    if let Some(agents) = value.get("agent").and_then(Value::as_object) {
        for (name, entry) in agents {
            match entry.get("disable").and_then(Value::as_bool) {
                Some(true) => {
                    if !layers.disabled_agents.contains(name) {
                        layers.disabled_agents.push(name.clone());
                    }
                    continue;
                }
                Some(false) => layers.disabled_agents.retain(|agent| agent != name),
                None => {}
            }
            let prompt = entry.get("prompt").and_then(Value::as_str).unwrap_or("");
            if prompt.is_empty() {
                continue;
            }
            #[allow(clippy::cast_possible_truncation)]
            let number = |key: &str| {
                entry
                    .get(key)
                    .and_then(Value::as_f64)
                    .map(|value| value as f32)
            };
            upsert_agent(
                &mut layers.agents,
                CustomAgent {
                    id: name.clone(),
                    description: entry
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("Custom agent")
                        .to_owned(),
                    prompt: prompt.to_owned(),
                    mode: entry.get("mode").and_then(Value::as_str).map(str::to_owned),
                    model: entry
                        .get("model")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    tools: agent_tools(entry.get("tools")),
                    permissions: agent_permissions(
                        entry.get("permission").or_else(|| entry.get("permissions")),
                        path,
                        &format!("agent.{name}.permission"),
                    ),
                    hidden: entry
                        .get("hidden")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    color: entry
                        .get("color")
                        .and_then(Value::as_str)
                        .and_then(agent_color),
                    steps: entry
                        .get("steps")
                        .or_else(|| entry.get("maxSteps"))
                        .and_then(Value::as_u64)
                        .and_then(|value| u32::try_from(value).ok())
                        .filter(|value| *value > 0),
                    temperature: number("temperature"),
                    top_p: number("top_p"),
                    variant: entry
                        .get("variant")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                },
            );
        }
    }
    if let Some(commands) = value.get("command").and_then(Value::as_object) {
        for (name, entry) in commands {
            let template = entry.get("template").and_then(Value::as_str).unwrap_or("");
            if template.is_empty() {
                continue;
            }
            upsert_command(
                &mut layers.commands,
                CommandInfo {
                    execution: haarness_protocol::DefinitionExecution {
                        fork: entry.get("subtask").and_then(Value::as_bool) == Some(true),
                        ..Default::default()
                    },
                    name: name.clone(),
                    description: entry
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    source: None,
                    agent: entry
                        .get("agent")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    model: entry
                        .get("model")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    argument_hint: None,
                    template: template.to_owned(),
                    plugin: None,
                },
            );
        }
    }
}

/// `permission` configuration: a bare action, or a map of
/// tool → action | { pattern → action }, declaration order preserved. An
/// action is `allow`, `ask` or `deny` in any letter case (`"Deny"` is what
/// was meant); any other value leaves its rule out, with a warning in the log
/// naming `file` and the key (`key` is where the block sits: `permission`,
/// `agent.review.permission`).
fn compile_permission(rules: &mut Vec<PermissionRule>, permission: &Value, file: &Path, key: &str) {
    let push =
        |rules: &mut Vec<PermissionRule>, pattern: String, action: &Value, at: &str| match action
            .as_str()
            .and_then(decision)
        {
            Some(decision) => rules.push(PermissionRule { pattern, decision }),
            None => ignored_action(file, at, action),
        };
    match permission {
        Value::Object(map) => {
            for (tool, entry) in map {
                let tool_pattern = |pattern: &str| {
                    if tool == "*" && pattern == "*" {
                        "*".to_owned()
                    } else if pattern == "*" {
                        format!("{tool}:*")
                    } else {
                        format!("{tool}:{pattern}")
                    }
                };
                match entry {
                    Value::Object(patterns) => {
                        for (pattern, action) in patterns {
                            push(
                                rules,
                                tool_pattern(pattern),
                                action,
                                &format!("{key}.{tool}.{pattern}"),
                            );
                        }
                    }
                    action => push(rules, tool_pattern("*"), action, &format!("{key}.{tool}")),
                }
            }
        }
        action => push(rules, "*".into(), action, key),
    }
}

/// A permission action [`compile_permission`] cannot read, logged once per
/// file, key and value for the process (configuration loads again on every
/// reload).
fn ignored_action(file: &Path, key: &str, action: &Value) {
    static SEEN: std::sync::LazyLock<std::sync::Mutex<HashSet<String>>> =
        std::sync::LazyLock::new(Default::default);
    let message = format!(
        "config: ignored \"{key}\" in {}: {action} is not allow, ask or deny",
        file.display()
    );
    if SEEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(message.clone())
    {
        tracing::warn!("{message}");
    }
}

fn decision(action: &str) -> Option<PermissionDecision> {
    match action.to_ascii_lowercase().as_str() {
        "allow" => Some(PermissionDecision::Allow),
        "ask" => Some(PermissionDecision::Ask),
        "deny" => Some(PermissionDecision::Deny),
        _ => None,
    }
}

fn agent_tools(value: Option<&Value>) -> HashMap<String, bool> {
    value
        .and_then(Value::as_object)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|(name, enabled)| {
                    enabled.as_bool().map(|enabled| (name.clone(), enabled))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn agent_permissions(value: Option<&Value>, file: &Path, key: &str) -> Vec<PermissionRule> {
    let Some(value) = value else {
        return Vec::new();
    };
    if let Ok(rules) = serde_json::from_value::<Vec<PermissionRule>>(value.clone()) {
        return rules;
    }
    let mut rules = Vec::new();
    compile_permission(&mut rules, value, file, key);
    rules
}

/// Haarness MCP entry: `{type: "local"|"remote", command: [..], url, enabled,
/// environment}` — or the Claude-style `{command, args}` shape — as written:
/// `${VAR}` references stay text, so what is shown, edited and written back
/// keeps them instead of the values they hold.
pub(super) fn mcp_entry(entry: &Value) -> Option<McpConfigureRequest> {
    mcp_entry_with_root(entry, None)
}

/// The entry a configuration load registers: plugin-root placeholders are
/// filled, then [`resolve_mcp_entry`] resolves references once.
fn loaded_mcp_entry(entry: &Value, plugin_root: Option<&Path>) -> Option<McpConfigureRequest> {
    mcp_entry_with_root(entry, plugin_root).map(resolve_mcp_entry)
}

/// Resolves `${VAR}` references in the strings the MCP client uses as given
/// (command, arguments, folder, URL, OAuth client and scopes). Environment
/// values and headers keep theirs: the client resolves them once when it
/// connects, so a variable whose value holds `${OTHER}` is not read again,
/// and the server receives that text as it is. So does the headers helper,
/// a shell command: its shell reads each reference from its environment, so
/// a value is never read as shell syntax.
pub(super) fn resolve_mcp_entry(mut entry: McpConfigureRequest) -> McpConfigureRequest {
    for value in entry
        .command
        .iter_mut()
        .chain(&mut entry.args)
        .chain(&mut entry.cwd)
        .chain(&mut entry.url)
        .chain(&mut entry.oauth_client_id)
        .chain(&mut entry.oauth_scopes)
    {
        *value = expand_env(value);
    }
    entry
}

fn mcp_entry_with_root(entry: &Value, plugin_root: Option<&Path>) -> Option<McpConfigureRequest> {
    let enabled = entry
        .get("enabled")
        .and_then(Value::as_bool)
        .or_else(|| {
            entry
                .get("disabled")
                .and_then(Value::as_bool)
                .map(|disabled| !disabled)
        })
        .unwrap_or(true);
    let expand = |value: &str| plugin_placeholders(value, plugin_root);
    // opencode writes `timeout` in milliseconds (its entries say `type:
    // "local"` or `"remote"`); a Claude Code or plugin entry writes seconds.
    let opencode = matches!(
        entry.get("type").and_then(Value::as_str),
        Some("local" | "remote")
    );
    let timeout_ms = entry
        .get("timeout")
        .and_then(Value::as_u64)
        .map(|timeout| {
            if opencode {
                timeout
            } else {
                timeout.saturating_mul(1_000)
            }
        })
        .or_else(|| entry.get("timeoutMs").and_then(Value::as_u64));
    if let Some(url) = entry.get("url").and_then(Value::as_str) {
        let kind = entry
            .get("transport")
            .or_else(|| entry.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("http")
            .to_ascii_lowercase();
        return Some(McpConfigureRequest {
            transport: if kind == "sse" {
                "sse".into()
            } else {
                "streamable_http".into()
            },
            command: None,
            args: Vec::new(),
            env: plugin_mcp_env(
                mcp_string_map_with(
                    entry.get("environment").or_else(|| entry.get("env")),
                    &expand,
                ),
                plugin_root,
            ),
            cwd: entry.get("cwd").and_then(Value::as_str).map(&expand),
            url: Some(expand(url)),
            headers: mcp_string_map_with(entry.get("headers"), &expand),
            // A shell command: the plugin root stays `${CLAUDE_PLUGIN_ROOT}`
            // for the shell to read from `env` below, never pasted in.
            headers_helper: entry
                .get("headersHelper")
                .and_then(Value::as_str)
                .map(str::to_owned),
            timeout_ms,
            // opencode writes `oauth` as `false` or an object with its
            // `clientId` and space-separated `scope`; an object auto-detects.
            oauth: entry.get("oauth").and_then(Value::as_bool),
            oauth_client_id: entry
                .pointer("/auth/clientId")
                .or_else(|| entry.pointer("/oauth/clientId"))
                .or_else(|| entry.get("oauthClientId"))
                .and_then(Value::as_str)
                .map(&expand),
            oauth_scopes: match entry.pointer("/oauth/scope").and_then(Value::as_str) {
                Some(scope) => scope.split_whitespace().map(&expand).collect(),
                None => entry
                    .pointer("/auth/scopes")
                    .or_else(|| entry.get("oauthScopes"))
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(&expand)
                    .collect(),
            },
            enabled,
            apps: entry.get("apps").and_then(Value::as_bool),
        });
    }
    let command = entry.get("command")?;
    let (command, args) = match command {
        Value::String(command) => (
            expand(command),
            entry
                .get("args")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(&expand)
                        .collect()
                })
                .unwrap_or_default(),
        ),
        Value::Array(parts) => {
            let mut parts = parts.iter().filter_map(Value::as_str).map(&expand);
            let command = parts.next()?;
            let mut args: Vec<_> = parts.collect();
            args.extend(
                entry
                    .get("args")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(&expand),
            );
            (command, args)
        }
        _ => return None,
    };
    Some(McpConfigureRequest {
        transport: "stdio".into(),
        command: Some(command),
        args,
        env: plugin_mcp_env(
            mcp_string_map_with(
                entry.get("environment").or_else(|| entry.get("env")),
                &expand,
            ),
            plugin_root,
        ),
        cwd: entry.get("cwd").and_then(Value::as_str).map(&expand),
        url: None,
        headers: BTreeMap::new(),
        headers_helper: None,
        timeout_ms,
        oauth: None,
        oauth_client_id: None,
        oauth_scopes: Vec::new(),
        enabled,
        apps: entry.get("apps").and_then(Value::as_bool),
    })
}

fn mcp_string_map_with(
    value: Option<&Value>,
    expand: &impl Fn(&str) -> String,
) -> BTreeMap<String, String> {
    value
        .and_then(Value::as_object)
        .map(|values| {
            values
                .iter()
                .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), expand(value))))
                .collect()
        })
        .unwrap_or_default()
}

fn plugin_mcp_env(
    mut env: BTreeMap<String, String>,
    plugin_root: Option<&Path>,
) -> BTreeMap<String, String> {
    if let Some(root) = plugin_root {
        let root = root.display().to_string();
        env.insert("HAARNESS_PLUGIN_ROOT".into(), root.clone());
        env.insert("CLAUDE_PLUGIN_ROOT".into(), root.clone());
        env.insert("OPENCODE_PLUGIN_ROOT".into(), root);
    }
    env
}

fn plugin_placeholders(value: &str, plugin_root: Option<&Path>) -> String {
    plugin_root.map_or_else(
        || value.to_owned(),
        |root| {
            let root = root.display().to_string();
            value
                .replace("${HAARNESS_PLUGIN_ROOT}", &root)
                .replace("${OPENCODE_PLUGIN_ROOT}", &root)
                .replace("${CLAUDE_PLUGIN_ROOT}", &root)
        },
    )
}

/// `${VAR}` / `${VAR:-default}` expansion (the Claude `.mcp.json` idiom).
fn expand_env(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let token = &after[..end];
        let (name, fallback) = token
            .split_once(":-")
            .map_or((token, None), |(name, fallback)| (name, Some(fallback)));
        match std::env::var(name) {
            Ok(resolved) => out.push_str(&resolved),
            Err(_) => out.push_str(fallback.unwrap_or_default()),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// MCP definitions may be inline in `.claude-plugin/plugin.json`, referenced
/// from it by one or more relative files, or supplied by the conventional
/// `.mcp.json` fallback. Project/global MCP definitions retain precedence.
fn apply_plugin_mcp(layers: &mut JsonLayers, root: &Path) {
    let manifest_path = root.join(".claude-plugin/plugin.json");
    let manifest = read_json_recording(layers, &manifest_path);
    let mut refs = Vec::new();
    match manifest.as_ref().and_then(|value| value.get("mcpServers")) {
        Some(Value::String(path)) => refs.push(path.clone()),
        Some(Value::Array(paths)) => {
            refs.extend(paths.iter().filter_map(Value::as_str).map(str::to_owned))
        }
        Some(Value::Object(servers)) => apply_plugin_mcp_servers(layers, servers, root),
        _ => {}
    }
    if root.join(".mcp.json").is_file() {
        refs.push(".mcp.json".into());
    }
    refs.sort();
    refs.dedup();
    for reference in refs {
        let Some(path) = plugin_reference(root, &reference) else {
            layers.notices.push(format!(
                "ignored plugin MCP path outside {}: {reference}",
                root.display()
            ));
            continue;
        };
        let Some(value) = read_json_recording(layers, &path) else {
            layers.notices.push(format!(
                "ignored unreadable plugin MCP file {}",
                path.display()
            ));
            continue;
        };
        let servers = value
            .get("mcpServers")
            .and_then(Value::as_object)
            .or_else(|| value.as_object());
        if let Some(servers) = servers {
            apply_plugin_mcp_servers(layers, servers, root);
        }
    }
}

fn apply_plugin_mcp_servers(
    layers: &mut JsonLayers,
    servers: &serde_json::Map<String, Value>,
    root: &Path,
) {
    for (name, entry) in servers {
        if let Some(request) = loaded_mcp_entry(entry, Some(root)) {
            layers.mcp.entry(name.clone()).or_insert(request);
        }
    }
}

fn plugin_reference(root: &Path, reference: &str) -> Option<PathBuf> {
    if reference.trim().is_empty() || Path::new(reference).is_absolute() {
        return None;
    }
    let target = root.join(reference.trim_start_matches("./"));
    let canonical_root = root.canonicalize().ok()?;
    let canonical_target = target.canonicalize().ok()?;
    canonical_target
        .starts_with(canonical_root)
        .then_some(canonical_target)
}

fn upsert_command(commands: &mut Vec<CommandInfo>, command: CommandInfo) {
    if let Some(existing) = commands
        .iter_mut()
        .find(|existing| existing.name == command.name)
    {
        *existing = command;
    } else {
        commands.push(command);
    }
}

fn upsert_agent(agents: &mut Vec<CustomAgent>, agent: CustomAgent) {
    if let Some(existing) = agents.iter_mut().find(|existing| existing.id == agent.id) {
        *existing = agent;
    } else {
        agents.push(agent);
    }
}

/// A definition file whose frontmatter does not parse is skipped and reported,
/// so a command, skill or agent that never appears has a recorded cause.
fn definition_markdown<'a>(
    content: &'a str,
    path: &Path,
    kind: &str,
    notices: &mut Vec<String>,
) -> Option<(BTreeMap<String, String>, &'a str)> {
    match haarness_core::definitions::markdown(content) {
        Ok(parsed) => Some(parsed),
        Err(error) => {
            let error = match error {
                haarness_core::CoreError::Invalid(message) => message,
                error => error.to_string(),
            };
            notices.push(format!("skipped {kind} {}: {error}", path.display()));
            None
        }
    }
}

/// Markdown slash commands: `<root>/**/*.md`, frontmatter `description`,
/// `agent`, `model`, `argument-hint`; nested directories namespace the name
/// (`git/review.md` → `git/review`).
fn discover_commands(
    commands: &mut Vec<CommandInfo>,
    notices: &mut Vec<String>,
    root: &Path,
    base: &Path,
    plugin: Option<&str>,
) {
    let paths = if root.is_file() {
        vec![root.to_path_buf()]
    } else {
        fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect()
    };
    for path in paths {
        if path.is_dir() {
            discover_commands(commands, notices, &path, base, plugin);
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let Some((front, body)) = definition_markdown(&content, &path, "command", notices) else {
            continue;
        };
        let relative_name = path
            .strip_prefix(if base.is_file() {
                base.parent().unwrap_or(base)
            } else {
                base
            })
            .unwrap_or(&path)
            .with_extension("")
            .to_string_lossy()
            .replace('\\', "/");
        let name = plugin.map_or_else(
            || relative_name.replace("%3A", ":"),
            |plugin| format!("{plugin}:{relative_name}"),
        );
        upsert_command(
            commands,
            CommandInfo {
                execution: haarness_core::definitions::execution(
                    &front,
                    &path,
                    haarness_core::definitions::plugin_root(&path).as_deref(),
                ),
                name,
                description: front.get("description").cloned().unwrap_or_default(),
                source: None,
                agent: front.get("agent").cloned(),
                model: front.get("model").cloned(),
                argument_hint: front
                    .get("argument-hint")
                    .or_else(|| front.get("argument_hint"))
                    .cloned(),
                template: body.trim().to_owned(),
                plugin: plugin.map(ToOwned::to_owned),
            },
        );
    }
}

fn plugin_name(root: &Path) -> Option<String> {
    let manifest = read_json(&root.join(".claude-plugin/plugin.json"))
        .ok()
        .flatten();
    manifest
        .as_ref()
        .and_then(|value| value.get("name"))
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| root.file_name().map(|name| name.to_string_lossy().into()))
}

pub(super) fn discover_skill_commands(
    commands: &mut Vec<CommandInfo>,
    notices: &mut Vec<String>,
    root: &Path,
    plugin: Option<&str>,
) {
    discover_skill_commands_inner(commands, notices, root, plugin, 0);
}
fn discover_skill_commands_inner(
    commands: &mut Vec<CommandInfo>,
    notices: &mut Vec<String>,
    root: &Path,
    plugin: Option<&str>,
    depth: usize,
) {
    if depth > 16 {
        return;
    }
    let paths = if root.is_file() {
        vec![root.to_path_buf()]
    } else if root.join("SKILL.md").is_file() {
        vec![root.join("SKILL.md")]
    } else {
        fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect()
    };
    for path in paths {
        if path.is_dir() {
            discover_skill_commands_inner(commands, notices, &path, plugin, depth + 1);
            continue;
        }
        if path.file_name().is_none_or(|n| n != "SKILL.md") {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let Some((front, body)) = definition_markdown(&content, &path, "skill", notices) else {
            continue;
        };
        let execution = haarness_core::definitions::execution(
            &front,
            &path,
            haarness_core::definitions::plugin_root(&path).as_deref(),
        );
        if !execution.user_invocable {
            continue;
        }
        let name = haarness_core::definitions::skill_name(&front, &path);
        if name.is_empty() {
            continue;
        }
        upsert_command(
            commands,
            CommandInfo {
                name: plugin.map_or_else(|| name.clone(), |p| format!("{p}:{name}")),
                description: front.get("description").cloned().unwrap_or_default(),
                source: Some("skill".into()),
                agent: execution.agent.clone(),
                model: execution.model.clone(),
                argument_hint: front
                    .get("argument-hint")
                    .cloned()
                    .or(Some("[request]".into())),
                template: body.trim().to_owned(),
                plugin: plugin.map(str::to_owned),
                execution,
            },
        );
    }
}

/// Markdown agents: `<root>/*.md`, frontmatter `name` (default: filename),
/// `description`, `model`, `mode`, `tools`, `permission`, plus the Haarness
/// presentation/limit fields `hidden`, `color`, `steps`/`maxSteps`,
/// `temperature`, `top_p`, `variant` and `disable`; the body is the system
/// prompt. Structured policy values use compact JSON in frontmatter.
fn discover_agents(
    agents: &mut Vec<CustomAgent>,
    notices: &mut Vec<String>,
    root: &Path,
    base: &Path,
    plugin: Option<&str>,
) {
    discover_agents_with(agents, notices, root, base, plugin, false);
}

/// `claude` normalises Claude Code agent frontmatter: `allowed-tools` as a
/// fallback for `tools`, colour names, and `model: inherit` meaning no pin
/// (the `sonnet`/`opus`/`haiku`/`fable` families and bare Claude ids such as
/// `claude-sonnet-5-5` resolve at dispatch time).
fn discover_agents_with(
    agents: &mut Vec<CustomAgent>,
    notices: &mut Vec<String>,
    root: &Path,
    base: &Path,
    plugin: Option<&str>,
    claude: bool,
) {
    let paths = if root.is_file() {
        vec![root.to_path_buf()]
    } else {
        fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect()
    };
    for path in paths {
        if path.is_dir() {
            discover_agents_with(agents, notices, &path, base, plugin, claude);
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let Some((mut front, body)) = definition_markdown(&content, &path, "agent", notices) else {
            continue;
        };
        if body.trim().is_empty() {
            continue;
        }
        if claude {
            if !front.contains_key("tools")
                && let Some(allowed) = front
                    .get("allowed-tools")
                    .or_else(|| front.get("allowed_tools"))
                    .cloned()
            {
                front.insert("tools".into(), allowed);
            }
            if front
                .get("model")
                .is_some_and(|model| model.trim() == "inherit")
            {
                front.remove("model");
            }
        }
        let relative = path
            .strip_prefix(if base.is_file() {
                base.parent().unwrap_or(base)
            } else {
                base
            })
            .unwrap_or(&path)
            .with_extension("")
            .to_string_lossy()
            .replace('\\', "/");
        let explicit = front.get("name").filter(|name| !name.trim().is_empty());
        let id = plugin.map_or_else(
            || {
                explicit
                    .cloned()
                    .unwrap_or_else(|| relative.replace('/', ":"))
            },
            |plugin| scoped_agent_name(plugin, &relative, explicit.map(String::as_str)),
        );
        if id.is_empty() {
            continue;
        }
        if front.get("disable").is_some_and(|value| value == "true") {
            // Retire the name from earlier layers; a disabled definition
            // contributes nothing itself.
            agents.retain(|agent| agent.id != id);
            continue;
        }
        let number = |key: &str| {
            front
                .get(key)
                .and_then(|raw| raw.trim().parse::<f32>().ok())
        };
        upsert_agent(
            agents,
            CustomAgent {
                id,
                description: front.get("description").cloned().unwrap_or_default(),
                prompt: body.trim().to_owned(),
                mode: front.get("mode").cloned(),
                model: front.get("model").cloned(),
                tools: markdown_agent_tools(&front),
                permissions: front
                    .get("permission")
                    .or_else(|| front.get("permissions"))
                    .and_then(|raw| serde_json::from_str(raw).ok())
                    .map_or_else(Vec::new, |value| {
                        agent_permissions(Some(&value), &path, "permission")
                    }),
                hidden: front.get("hidden").is_some_and(|value| value == "true"),
                color: front.get("color").map(String::as_str).and_then(agent_color),
                steps: front
                    .get("steps")
                    .or_else(|| front.get("maxSteps"))
                    .and_then(|raw| raw.trim().parse::<u32>().ok())
                    .filter(|value| *value > 0),
                temperature: number("temperature"),
                top_p: number("top_p"),
                variant: front.get("variant").cloned(),
            },
        );
    }
}

/// Agent colour: a `#rrggbb` value or Haarness theme name passes through;
/// Claude Code colour names map onto the theme names (config/claude.ts).
fn agent_color(raw: &str) -> Option<String> {
    let value = raw.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }
    if matches!(
        value.as_str(),
        "primary" | "secondary" | "accent" | "success" | "warning" | "error" | "info"
    ) {
        return Some(value);
    }
    if value.len() == 7
        && value.starts_with('#')
        && value[1..].chars().all(|c| c.is_ascii_hexdigit())
    {
        return Some(value);
    }
    let theme = match value.as_str() {
        "red" => "error",
        "green" => "success",
        "yellow" | "orange" => "warning",
        "blue" | "cyan" => "info",
        "pink" | "magenta" | "purple" => "accent",
        "gray" | "grey" => "secondary",
        _ => return None,
    };
    Some(theme.to_owned())
}

fn scoped_agent_name(plugin: &str, relative: &str, explicit: Option<&str>) -> String {
    let explicit = explicit.map(|name| name.trim().replace('/', ":"));
    if explicit
        .as_deref()
        .is_some_and(|name| name.starts_with(&format!("{plugin}:")))
    {
        return explicit.unwrap_or_default();
    }
    if explicit.as_deref().is_some_and(|name| name.contains(':')) {
        return format!("{plugin}:{}", explicit.unwrap_or_default());
    }
    let mut parts = relative
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if let Some(explicit) = explicit.as_deref()
        && let Some(leaf) = parts.last_mut()
    {
        *leaf = explicit;
    }
    format!("{plugin}:{}", parts.join(":"))
}

fn markdown_agent_tools(front: &BTreeMap<String, String>) -> HashMap<String, bool> {
    let mut tools = HashMap::new();
    if let Some(raw) = front.get("tools") {
        if let Ok(value @ Value::Object(_)) = serde_json::from_str::<Value>(raw) {
            tools = agent_tools(Some(&value));
        } else {
            tools.insert("*".into(), false);
            for name in haarness_core::definitions::list(Some(raw)) {
                for name in haarness_core::definitions::tool_names(&name) {
                    tools.insert(name, true);
                }
            }
        }
    }
    for name in haarness_core::definitions::list(front.get("disallowedTools")) {
        for name in haarness_core::definitions::tool_names(&name) {
            tools.insert(name, false);
        }
    }
    tools
}

/// One-time migration: an existing `config.toml` without a sibling
/// `haarness.json` is translated (defaults + permissions) and renamed.
pub fn migrate_toml(config_toml: &Path, notices: &mut Vec<String>) -> Result<()> {
    let target = config_toml.with_file_name("haarness.json");
    if !config_toml.exists() || target.exists() {
        return Ok(());
    }
    let parsed: toml::Value = toml::from_str(&fs::read_to_string(config_toml)?)?;
    // Only migrate configs the JSON layer fully expresses; anything with
    // provider/MCP/compaction sections keeps working from TOML untouched.
    if let Some(table) = parsed.as_table() {
        if table
            .keys()
            .any(|key| !matches!(key.as_str(), "defaults" | "permissions"))
        {
            notices.push(format!(
                "{} kept as TOML (sections beyond defaults/permissions)",
                config_toml.display()
            ));
            return Ok(());
        }
    }
    let mut json = serde_json::Map::new();
    json.insert(
        "$schema".into(),
        Value::String("https://haarness.ai/schema/config.json".into()),
    );
    if let Some(defaults) = parsed.get("defaults").and_then(toml::Value::as_table) {
        if let (Some(provider), Some(model)) = (
            defaults.get("provider").and_then(toml::Value::as_str),
            defaults.get("model").and_then(toml::Value::as_str),
        ) {
            json.insert("model".into(), Value::String(format!("{provider}/{model}")));
        }
        if let Some(small) = defaults.get("small_model").and_then(toml::Value::as_str) {
            json.insert("small_model".into(), Value::String(small.into()));
        }
        if let Some(agent) = defaults.get("agent").and_then(toml::Value::as_str) {
            json.insert("default_agent".into(), Value::String(agent.into()));
        }
    }
    if let Some(permissions) = parsed.get("permissions").and_then(toml::Value::as_table) {
        let mut block = serde_json::Map::new();
        for (tool, entry) in permissions {
            match entry {
                toml::Value::String(action) => {
                    block.insert(tool.clone(), Value::String(action.clone()));
                }
                toml::Value::Table(patterns) => {
                    let mut inner = serde_json::Map::new();
                    for (pattern, action) in patterns {
                        if let Some(action) = action.as_str() {
                            inner.insert(pattern.clone(), Value::String(action.into()));
                        }
                    }
                    block.insert(tool.clone(), Value::Object(inner));
                }
                _ => {}
            }
        }
        if !block.is_empty() {
            json.insert("permission".into(), Value::Object(block));
        }
    }
    fs::write(&target, serde_json::to_string_pretty(&Value::Object(json))?)?;
    let retired = config_toml.with_file_name("config.toml.migrated");
    let _ = fs::rename(config_toml, &retired);
    notices.push(format!(
        "migrated {} to {}",
        config_toml.display(),
        target.display()
    ));
    Ok(())
}

/// Update one named setting while retaining sibling comments and formatting.
pub(super) fn write_entry(
    path: &Path,
    section: &str,
    name: &str,
    value: Option<Value>,
) -> Result<()> {
    write_entries(path, section, vec![(name.to_owned(), value)])
}

pub(super) fn write_entries(
    path: &Path,
    section: &str,
    updates: Vec<(String, Option<Value>)>,
) -> Result<()> {
    use anyhow::Context as _;
    edit_json_file(path, |object| {
        let entries = object
            .object_value_or_create(section)
            .context("configuration section must be an object")?;
        for (name, value) in updates {
            match (entries.get(&name), value) {
                (Some(entry), Some(value)) => entry.set_value(cst_input(value)),
                (None, Some(value)) => {
                    entries.append(&name, cst_input(value));
                }
                (Some(entry), None) => entry.remove(),
                (None, None) => {
                    anyhow::bail!("no {section} entry named '{name}' in {}", path.display())
                }
            }
        }
        Ok(())
    })
}

/// Sets only `fields` of the `name` entry of `section` (`None` removes a
/// key), leaving every other key of that entry, and the file's comments and
/// formatting, as written. A key written twice on the way is refused: the
/// edit would land on the first copy while the last one is in force.
pub(super) fn write_entry_fields(
    path: &Path,
    section: &str,
    name: &str,
    fields: Vec<(String, Option<Value>)>,
) -> Result<()> {
    use anyhow::Context as _;
    let repeated = |object: &jsonc_parser::cst::CstObject, key: &str| {
        object
            .properties()
            .iter()
            .filter(|property| {
                property
                    .name()
                    .and_then(|name| name.decoded_value().ok())
                    .as_deref()
                    == Some(key)
            })
            .count()
            > 1
    };
    edit_json_file(path, |object| {
        anyhow::ensure!(
            !repeated(object, section),
            "\"{section}\" is written more than once in {}; remove the extra copy first",
            path.display()
        );
        let entries = object
            .object_value(section)
            .with_context(|| format!("no {section} section in {}", path.display()))?;
        anyhow::ensure!(
            !repeated(&entries, name),
            "\"{section}.{name}\" is written more than once in {}; remove the extra copy first",
            path.display()
        );
        let entry = entries
            .object_value(name)
            .with_context(|| format!("no {section} entry named '{name}' in {}", path.display()))?;
        for (key, value) in fields {
            anyhow::ensure!(
                !repeated(&entry, &key),
                "\"{section}.{name}.{key}\" is written more than once in {}; remove the extra copy first",
                path.display()
            );
            match (entry.get(&key), value) {
                (Some(property), Some(value)) => property.set_value(cst_input(value)),
                (None, Some(value)) => {
                    entry.append(&key, cst_input(value));
                }
                (Some(property), None) => property.remove(),
                (None, None) => {}
            }
        }
        Ok(())
    })
}

fn cst_input(value: Value) -> jsonc_parser::cst::CstInputValue {
    use jsonc_parser::cst::CstInputValue;
    match value {
        Value::Null => CstInputValue::Null,
        Value::Bool(v) => CstInputValue::Bool(v),
        Value::Number(v) => CstInputValue::Number(v.to_string()),
        Value::String(v) => CstInputValue::String(v),
        Value::Array(v) => CstInputValue::Array(v.into_iter().map(cst_input).collect()),
        Value::Object(v) => CstInputValue::Object(
            v.into_iter()
                .map(|(key, value)| (key, cst_input(value)))
                .collect(),
        ),
    }
}

/// Edits a JSON or JSONC configuration file in place under the folder's
/// writer lock, keeping its comments and formatting, and replaces it
/// atomically with owner-only permissions.
fn edit_json_file(
    path: &Path,
    edit: impl FnOnce(&jsonc_parser::cst::CstObject) -> Result<()>,
) -> Result<()> {
    use anyhow::Context as _;
    use jsonc_parser::cst::CstRootNode;
    let parent = path.parent().context("configuration parent")?;
    fs::create_dir_all(parent)?;
    // Writers lock the folder itself: a lock file beside a project's
    // haarness.json would be left behind in the person's repository.
    #[cfg(unix)]
    let lock = fs::File::open(parent)?;
    #[cfg(not(unix))]
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.with_extension("write.lock"))?;
    fs2::FileExt::lock_exclusive(&lock)?;
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "{\n}\n".into(),
        Err(e) => return Err(e.into()),
    };
    let root = CstRootNode::parse(&text, &jsonc_parser::ParseOptions::default())?;
    let object = root
        .object_value_or_create()
        .context("configuration must be an object")?;
    edit(&object)?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::now_v7()));
    let result = (|| -> Result<()> {
        use std::io::Write as _;
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(root.to_string().as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    let _ = fs::remove_file(temporary);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal HTTP/1.1 server on a random loopback port that answers
    /// each `(path, body)` route and 404s everything else. Runs until the
    /// test process exits.
    fn serve(routes: Vec<(&'static str, &'static str)>) -> String {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => request.extend_from_slice(&buffer[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&request);
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let response = match routes.iter().find(|(route, _)| *route == path) {
                    Some((_, body)) => format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    ),
                    None => {
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_owned()
                    }
                };
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{address}")
    }

    /// Every file under `root` with its bytes, to prove nothing was written.
    fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(path) = stack.pop() {
            if path.is_dir() {
                stack.extend(
                    fs::read_dir(&path)
                        .unwrap()
                        .flatten()
                        .map(|entry| entry.path()),
                );
            } else if let Ok(bytes) = fs::read(&path) {
                files.insert(path, bytes);
            }
        }
        files
    }

    #[test]
    fn a_project_may_only_make_auto_review_stricter() {
        let root =
            std::env::temp_dir().join(format!("haarness-auto-review-{}", uuid::Uuid::now_v7()));
        let config = root.join("config");
        let workspace = root.join("workspace");
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(workspace.join(".git")).unwrap();
        fs::write(
            config.join("haarness.json"),
            r#"{"auto_review": {
                "model": "anthropic/claude-haiku",
                "environment": ["origin is our GitHub org"],
                "allow": ["Pushing feature branches"],
                "block": ["Deploys"],
                "timeout": 30
            }}"#,
        )
        .unwrap();
        fs::write(
            workspace.join("haarness.json"),
            r#"{"auto_review": {
                "enabled": false,
                "model": "attacker/model",
                "allow": ["Everything"],
                "block": ["Publishing packages"],
                "timeout": 10
            }}"#,
        )
        .unwrap();
        let layers = load_layers_with_base(&config, None, None, &workspace, true, &[]);
        let review = &layers.auto_review;
        assert!(review.enabled, "a project cannot turn the reviewer off");
        assert_eq!(review.model.as_deref(), Some("anthropic/claude-haiku"));
        assert_eq!(review.environment, vec!["origin is our GitHub org"]);
        assert_eq!(review.allow, vec!["Pushing feature branches"]);
        assert_eq!(review.block, vec!["Deploys"]);
        assert_eq!(review.project_block, vec!["Publishing packages"]);
        assert_eq!(review.timeout, Duration::from_secs(10));
        assert!(
            layers.notices.iter().any(|notice| notice
                .contains("ignored auto_review.enabled, auto_review.model, auto_review.allow")),
            "{:?}",
            layers.notices
        );
        // A project cannot raise the timeout either; without project
        // configuration none of it applies.
        fs::write(
            workspace.join("haarness.json"),
            r#"{"auto_review": {"timeout": 300}}"#,
        )
        .unwrap();
        let layers = load_layers_with_base(&config, None, None, &workspace, true, &[]);
        assert_eq!(layers.auto_review.timeout, Duration::from_secs(30));
        // A project's small_model is the project's chats' default, never the
        // reviewer's; the person's own is.
        fs::write(
            workspace.join("haarness.json"),
            r#"{"small_model": "openai/gpt-5", "auto_review": {"model": "openai/gpt-5"}}"#,
        )
        .unwrap();
        let layers = load_layers_with_base(&config, None, None, &workspace, true, &[]);
        assert_eq!(layers.small_model.as_deref(), Some("openai/gpt-5"));
        assert_eq!(layers.auto_review.small_model, None);
        assert_eq!(
            layers.auto_review.model.as_deref(),
            Some("anthropic/claude-haiku")
        );
        fs::write(
            config.join("config.toml"),
            "[defaults]\nsmall_model = \"anthropic/claude-haiku\"\n",
        )
        .unwrap();
        let layers = load_layers_with_base(&config, None, None, &workspace, true, &[]);
        assert_eq!(
            layers.auto_review.small_model.as_deref(),
            Some("anthropic/claude-haiku")
        );
        fs::remove_file(config.join("config.toml")).unwrap();
        fs::write(
            config.join("haarness.json"),
            r#"{"small_model": "anthropic/claude-haiku"}"#,
        )
        .unwrap();
        let layers = load_layers_with_base(&config, None, None, &workspace, true, &[]);
        assert_eq!(
            layers.auto_review.small_model.as_deref(),
            Some("anthropic/claude-haiku")
        );
        assert_eq!(
            layers.auto_review.model, None,
            "a project never names the reviewer"
        );
        let layers = load_layers_with_base(&config, None, None, &workspace, false, &[]);
        assert!(layers.auto_review.project_block.is_empty());
        // Defaults: on, the chat's models, a minute.
        let empty = root.join("empty");
        fs::create_dir_all(&empty).unwrap();
        let layers = load_layers_with_base(&empty, None, None, &empty, false, &[]);
        assert_eq!(
            layers.auto_review,
            haarness_core::AutoReviewSettings::default()
        );
        assert!(layers.auto_review.enabled);
        assert_eq!(layers.auto_review.timeout, Duration::from_secs(60));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_base_folder_is_the_lowest_layer_and_is_never_written() {
        let root =
            std::env::temp_dir().join(format!("haarness-base-layer-{}", uuid::Uuid::now_v7()));
        let base_dir = root.join("team");
        let config = root.join("config");
        let workspace = root.join("workspace");
        for dir in [
            base_dir.join("agents"),
            base_dir.join("commands/git"),
            base_dir.join("skills/release-check"),
            config.join("agents"),
            workspace.join(".git"),
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(
            base_dir.join("haarness.jsonc"),
            r#"{
                // Team defaults
                "model": "team/base-model",
                "small_model": "team/base-small",
                "mcp": {"docs": {"url": "https://docs.example/mcp"}},
                "hooks": {"Stop": [{"hooks": [{"command": "echo base"}]}]},
                "instructions": ["/opt/team/rules.md"]
            }"#,
        )
        .unwrap();
        fs::write(
            base_dir.join("AGENTS.md"),
            "Team rule: run the release checklist.",
        )
        .unwrap();
        fs::write(
            base_dir.join("agents/reviewer.md"),
            "---\ndescription: Team reviewer\n---\nReview like the team.",
        )
        .unwrap();
        fs::write(
            base_dir.join("agents/release.md"),
            "---\ndescription: Release helper\n---\nCheck the release.",
        )
        .unwrap();
        fs::write(
            base_dir.join("commands/git/ship.md"),
            "---\ndescription: Ship it\n---\nShip $ARGUMENTS",
        )
        .unwrap();
        fs::write(
            base_dir.join("skills/release-check/SKILL.md"),
            "---\nname: release-check\ndescription: The team checklist\n---\n1. Changelog.",
        )
        .unwrap();
        fs::write(
            config.join("haarness.json"),
            r#"{"model":"mine/global-model"}"#,
        )
        .unwrap();
        fs::write(
            config.join("agents/reviewer.md"),
            "---\ndescription: My reviewer\n---\nReview my way.",
        )
        .unwrap();
        let before = snapshot(&base_dir);
        let base = crate::layout::BaseFolder::at(base_dir.clone(), &workspace);
        let layers = load_layers_with_base(&config, Some(&base), None, &workspace, true, &[]);
        assert!(layers.errors.is_empty(), "{:?}", layers.errors);
        // The global layer overrides the base folder key by key.
        assert_eq!(layers.model.as_deref(), Some("global-model"));
        assert_eq!(layers.small_model.as_deref(), Some("team/base-small"));
        assert_eq!(
            layers.mcp["docs"].url.as_deref(),
            Some("https://docs.example/mcp")
        );
        assert!(layers.base_mcp.contains("docs"));
        let hook = layers
            .hook_sources
            .iter()
            .find(|source| source.path.starts_with(&base_dir))
            .expect("the base folder's hooks are listed");
        assert!(hook.base && hook.trusted && !hook.plugin);
        assert_eq!(layers.hooks.len(), 1);
        assert_eq!(layers.instruction_sources, ["/opt/team/rules.md"]);
        let reviewer = layers
            .agents
            .iter()
            .find(|agent| agent.id == "reviewer")
            .unwrap();
        assert_eq!(reviewer.prompt.trim(), "Review my way.");
        assert!(layers.agents.iter().any(|agent| agent.id == "release"));
        assert!(
            layers
                .commands
                .iter()
                .any(|command| command.name == "git/ship")
        );
        assert!(
            layers
                .commands
                .iter()
                .any(|command| command.name == "release-check")
        );
        let instructions = layers.instructions.unwrap();
        assert!(instructions.contains("Team rule: run the release checklist."));
        assert_eq!(snapshot(&base_dir), before, "the base folder was written");
        // Without a base folder nothing of it is read.
        let without = load_layers_with_base(&config, None, None, &workspace, true, &[]);
        assert!(without.agents.iter().all(|agent| agent.id != "release"));
        assert!(!without.mcp.contains_key("docs"));
        assert!(without.instructions.is_none());
        let (agents, commands) = base_definitions(&base);
        assert_eq!(agents.len(), 2);
        assert!(commands.iter().any(|command| command.name == "git/ship"));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_link_from_the_base_folder_into_the_project_is_not_followed() {
        let root =
            std::env::temp_dir().join(format!("haarness-base-link-{}", uuid::Uuid::now_v7()));
        let base_dir = root.join("team");
        let config = root.join("config");
        let workspace = root.join("workspace");
        for dir in [
            base_dir.join("agents"),
            config.clone(),
            workspace.join(".git"),
            workspace.join("planted/agents"),
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(
            workspace.join("planted/haarness.json"),
            r#"{"hooks":{"Stop":[{"hooks":[{"command":"echo planted"}]}]}}"#,
        )
        .unwrap();
        fs::write(
            workspace.join("planted/agents/planted.md"),
            "---\ndescription: Planted\n---\nPlanted.",
        )
        .unwrap();
        std::os::unix::fs::symlink(
            workspace.join("planted/haarness.json"),
            base_dir.join("haarness.json"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            workspace.join("planted/agents/planted.md"),
            base_dir.join("agents/planted.md"),
        )
        .unwrap();
        // A plugin and a skill the repository planted, linked into the base
        // folder's plugins and skills.
        fs::create_dir_all(workspace.join("planted/plugin/.claude-plugin")).unwrap();
        fs::write(
            workspace.join("planted/plugin/.claude-plugin/plugin.json"),
            r#"{"name":"planted-plugin"}"#,
        )
        .unwrap();
        fs::create_dir_all(workspace.join("planted/skill")).unwrap();
        fs::write(
            workspace.join("planted/skill/SKILL.md"),
            "---\nname: planted-skill\ndescription: Planted\n---\nPLANTED",
        )
        .unwrap();
        fs::create_dir_all(base_dir.join("plugins/local")).unwrap();
        fs::create_dir_all(base_dir.join("skills/team")).unwrap();
        fs::create_dir_all(base_dir.join("plugins/local/team/.claude-plugin")).unwrap();
        fs::write(
            base_dir.join("plugins/local/team/.claude-plugin/plugin.json"),
            r#"{"name":"team-plugin"}"#,
        )
        .unwrap();
        fs::write(
            base_dir.join("skills/team/SKILL.md"),
            "---\nname: team-skill\ndescription: Team\n---\nTEAM",
        )
        .unwrap();
        std::os::unix::fs::symlink(
            workspace.join("planted/plugin"),
            base_dir.join("plugins/local/planted"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            workspace.join("planted/skill"),
            base_dir.join("skills/planted"),
        )
        .unwrap();
        let base = crate::layout::BaseFolder::at(base_dir.clone(), &workspace);
        // Project configuration off: nothing of the project may load.
        let layers = load_layers_with_base(&config, Some(&base), None, &workspace, false, &[]);
        assert!(layers.hooks.is_empty());
        assert!(layers.agents.iter().all(|agent| agent.id != "planted"));
        assert!(
            layers
                .commands
                .iter()
                .any(|command| command.name == "team-skill")
        );
        assert!(
            layers
                .commands
                .iter()
                .all(|command| command.name != "planted-skill")
        );
        // The base folder's own plugin and skill load; the linked ones never do.
        assert_eq!(
            crate::plugins::base_plugin_roots_in(&base),
            [base_dir.join("plugins/local/team")]
        );
        assert_eq!(base.skill_folders("skills"), [base_dir.join("skills/team")]);
        // A base folder the person points into the project stays theirs.
        let inside = crate::layout::BaseFolder::at(workspace.join("planted"), &workspace);
        let layers = load_layers_with_base(&config, Some(&inside), None, &workspace, false, &[]);
        assert_eq!(layers.hooks.len(), 1);
        assert!(layers.agents.iter().any(|agent| agent.id == "planted"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cloud_session_defaults_are_portable_and_project_overrides_still_win() {
        let root =
            std::env::temp_dir().join(format!("haarness-cloud-config-{}", uuid::Uuid::now_v7()));
        let config = root.join("config");
        let workspace = root.join("workspace");
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(workspace.join(".haarness/cloud/skills/review")).unwrap();
        fs::write(config.join("haarness.json"), r#"{"model":"router/global"}"#).unwrap();
        fs::write(
            workspace.join(".haarness/cloud/haarness.json"),
            r#"{
            "model":"router/cloud", "default_agent":"cloud-agent",
            "agent":{"cloud-agent":{"prompt":"cloud instructions"}},
            "mcp":{"docs":{"url":"https://docs.example/mcp"}},
            "skills":{"paths":[".haarness/cloud/skills"]}
        }"#,
        )
        .unwrap();
        let loaded = load_layers(&config, None, &workspace, true, &[]);
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        assert_eq!(loaded.model.as_deref(), Some("cloud"));
        assert_eq!(loaded.default_agent.as_deref(), Some("cloud-agent"));
        assert_eq!(
            loaded.mcp["docs"].url.as_deref(),
            Some("https://docs.example/mcp")
        );
        assert!(
            loaded
                .agents
                .iter()
                .any(|agent| agent.prompt == "cloud instructions")
        );
        assert_eq!(
            configured_skill_paths(&loaded.native, None, &workspace),
            vec![workspace.join(".haarness/cloud/skills")]
        );
        fs::write(
            workspace.join("haarness.json"),
            r#"{"model":"router/project","mcp":{"docs":{"url":"https://project.example/mcp"}}}"#,
        )
        .unwrap();
        let overridden = load_layers(&config, None, &workspace, true, &[]);
        assert_eq!(overridden.model.as_deref(), Some("project"));
        assert_eq!(
            overridden.mcp["docs"].url.as_deref(),
            Some("https://project.example/mcp")
        );
        let disabled = load_layers(&config, None, &workspace, false, &[]);
        assert_eq!(disabled.model.as_deref(), Some("global"));
        assert!(disabled.mcp.is_empty());
        assert!(disabled.agents.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn instruction_imports_leave_out_files_a_read_rule_denies_and_secrets() {
        let root = std::env::temp_dir().join(format!(
            "haarness-instruction-denied-{}",
            uuid::Uuid::now_v7()
        ));
        let config = root.join("config");
        let workspace = root.join("project");
        for dir in [
            config.clone(),
            workspace.join(".git"),
            workspace.join("private"),
            workspace.join("public"),
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(
            workspace.join("haarness.json"),
            r#"{"permission":{"read":{"private/*":"deny"}}}"#,
        )
        .unwrap();
        fs::write(workspace.join("private/notes.md"), "PRIVATE_BODY").unwrap();
        fs::write(workspace.join("public/notes.md"), "PUBLIC_BODY").unwrap();
        fs::write(workspace.join(".env"), "ENV_SECRET").unwrap();
        fs::write(
            workspace.join("AGENTS.md"),
            "@private/notes.md\n@public/notes.md\n@.env\n",
        )
        .unwrap();
        let layers = load_layers(&config, None, &workspace, true, &[]);
        let instructions = layers.instructions.expect("instructions");
        assert!(instructions.contains("PUBLIC_BODY"), "{instructions}");
        assert!(!instructions.contains("PRIVATE_BODY"), "{instructions}");
        assert!(!instructions.contains("ENV_SECRET"), "{instructions}");
        assert!(
            instructions
                .contains("[Instruction import omitted: private/notes.md (a read rule denies it)]")
        );
        assert!(instructions.contains("[Instruction import omitted: .env (it is a secret file)]"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remote_instruction_urls_hide_their_credentials_in_the_prompt_and_notices() {
        let root = std::env::temp_dir().join(format!(
            "haarness-instruction-credentials-{}",
            uuid::Uuid::now_v7()
        ));
        let config = root.join("config");
        let workspace = root.join("project");
        for dir in [config.clone(), workspace.join(".git")] {
            fs::create_dir_all(dir).unwrap();
        }
        let base = serve(vec![("/remote.md?token=PRIVATE_SUFFIX", "REMOTE_RULES")]);
        let reachable = format!(
            "{}/remote.md?token=PRIVATE_SUFFIX",
            base.replacen("http://", "http://fixture:PRIVATE_SUFFIX@", 1)
        );
        fs::write(
            config.join("haarness.json"),
            serde_json::json!({"instructions": [
                reachable,
                "http://fixture:PRIVATE_SUFFIX@127.0.0.1:1/down.md?api_key=PRIVATE_SUFFIX",
            ]})
            .to_string(),
        )
        .unwrap();
        let layers = load_layers(&config, None, &workspace, true, &[]);
        let instructions = layers.instructions.expect("instructions");
        assert!(instructions.contains("REMOTE_RULES"), "{instructions}");
        assert!(!instructions.contains("PRIVATE_SUFFIX"), "{instructions}");
        assert!(
            instructions.contains("/remote.md?token=<redacted>"),
            "{instructions}"
        );
        let skipped = layers
            .notices
            .iter()
            .find(|notice| notice.starts_with("skipped remote instructions"))
            .expect("a notice for the address that cannot be reached");
        assert!(!skipped.contains("PRIVATE_SUFFIX"), "{skipped}");
        assert!(
            skipped.contains("http://<redacted>@127.0.0.1:1/down.md?api_key=<redacted>"),
            "{skipped}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn configured_instruction_entries_resolve_globs_home_paths_and_urls() {
        let root = std::env::temp_dir().join(format!(
            "haarness-instruction-entries-{}",
            uuid::Uuid::now_v7()
        ));
        let config = root.join("config");
        let home = root.join("home");
        let project = root.join("project");
        let workspace = project.join("packages/app");
        for dir in [
            config.clone(),
            home.join("shared"),
            project.join(".git"),
            project.join("docs"),
            workspace.join("docs"),
            root.join("elsewhere"),
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(project.join("docs/style.md"), "ROOT_STYLE").unwrap();
        fs::write(workspace.join("docs/app.md"), "APP_DOCS").unwrap();
        fs::write(home.join("shared/rules.md"), "HOME_RULES").unwrap();
        fs::write(root.join("elsewhere/abs.md"), "ABSOLUTE_RULES").unwrap();
        fs::write(workspace.join("AGENTS.md"), "WORKSPACE_AGENTS").unwrap();
        let base = serve(vec![("/remote.md", "REMOTE_RULES")]);
        fs::write(
            config.join("haarness.json"),
            serde_json::json!({
                "instructions": ["~/shared/rules.md", format!("{base}/remote.md")]
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            workspace.join("haarness.json"),
            serde_json::json!({
                "instructions": [
                    "docs/*.md",
                    root.join("elsewhere/abs.md").display().to_string(),
                    "docs/*.md",
                    "http://127.0.0.1:1/unreachable.md",
                    "AGENTS.md"
                ]
            })
            .to_string(),
        )
        .unwrap();

        let layers = load_layers(&config, Some(&home), &workspace, true, &[]);
        assert!(layers.errors.is_empty(), "{:?}", layers.errors);
        let instructions = layers.instructions.expect("instructions");
        for expected in [
            "WORKSPACE_AGENTS",
            "HOME_RULES",
            "APP_DOCS",
            "ROOT_STYLE",
            "ABSOLUTE_RULES",
            "REMOTE_RULES",
        ] {
            assert_eq!(
                instructions.matches(expected).count(),
                1,
                "{expected}\n{instructions}"
            );
        }
        assert!(instructions.contains(&format!("<instructions source=\"{base}/remote.md\">")));
        assert!(
            layers.notices.iter().any(|notice| notice
                .contains("skipped remote instructions http://127.0.0.1:1/unreachable.md")),
            "{:?}",
            layers.notices
        );
        assert_eq!(
            layers.instruction_sources,
            vec![
                "~/shared/rules.md".to_owned(),
                format!("{base}/remote.md"),
                "docs/*.md".to_owned(),
                root.join("elsewhere/abs.md").display().to_string(),
                "http://127.0.0.1:1/unreachable.md".to_owned(),
                "AGENTS.md".to_owned(),
            ]
        );
        assert!(
            layers
                .instruction_files
                .contains(&workspace.join("docs/app.md"))
        );
        // Local files land before remote bodies.
        assert!(
            instructions.find("ABSOLUTE_RULES").unwrap()
                < instructions.find("REMOTE_RULES").unwrap()
        );

        // Without project configuration, relative entries are not globbed.
        let global_only = load_layers(&config, Some(&home), &workspace, false, &[]);
        let instructions = global_only.instructions.unwrap();
        assert!(instructions.contains("HOME_RULES"));
        assert!(!instructions.contains("APP_DOCS"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn claude_code_compatibility_is_opt_in_per_category() {
        let root =
            std::env::temp_dir().join(format!("haarness-claude-compat-{}", uuid::Uuid::now_v7()));
        let config = root.join("config");
        let home = root.join("home");
        let project = root.join("project");
        let workspace = project.join("service");
        for directory in [
            config.clone(),
            home.join(".claude/commands"),
            home.join(".claude/agents"),
            home.join(".claude/skills/home-skill"),
            project.join(".git"),
            project.join(".claude/commands"),
            project.join(".claude/agents"),
            project.join(".claude/skills/project-skill"),
            project.join(".haarness/commands"),
            workspace.join(".claude"),
        ] {
            fs::create_dir_all(directory).unwrap();
        }
        fs::write(home.join(".claude/CLAUDE.md"), "HOME_CLAUDE").unwrap();
        fs::write(home.join(".claude/commands/home-cmd.md"), "HOME_COMMAND").unwrap();
        fs::write(
            home.join(".claude/agents/scout.md"),
            "---\nname: scout\ndescription: Home scout\ncolor: red\ntools: Read, Grep\nmodel: inherit\n---\nScout the codebase.",
        )
        .unwrap();
        fs::write(
            home.join(".claude/skills/home-skill/SKILL.md"),
            "---\nname: home-skill\ndescription: Home skill\n---\nHOME_SKILL",
        )
        .unwrap();
        fs::write(project.join("AGENTS.md"), "PROJECT_AGENTS").unwrap();
        fs::write(project.join("CLAUDE.md"), "PROJECT_CLAUDE_LOSES").unwrap();
        fs::write(project.join("AGENTS.local.md"), "PROJECT_LOCAL").unwrap();
        fs::write(workspace.join(".claude/CLAUDE.md"), "WORKSPACE_CLAUDE").unwrap();
        fs::write(workspace.join("CONTEXT.md"), "WORKSPACE_CONTEXT_LOSES").unwrap();
        fs::write(project.join(".claude/commands/deploy.md"), "CLAUDE_DEPLOY").unwrap();
        fs::write(
            project.join(".haarness/commands/deploy.md"),
            "NATIVE_DEPLOY",
        )
        .unwrap();
        fs::write(project.join(".claude/commands/lint.md"), "CLAUDE_LINT").unwrap();
        fs::write(
            project.join(".claude/agents/fixer.md"),
            "---\ndescription: Fixer\nallowed-tools: [Edit, Bash]\ncolor: \"#123ABC\"\nmodel: sonnet\n---\nFix things.",
        )
        .unwrap();
        fs::write(
            project.join(".claude/skills/project-skill/SKILL.md"),
            "---\nname: project-skill\ndescription: Project skill\n---\nPROJECT_SKILL",
        )
        .unwrap();

        // Off by default: nothing from .claude is read.
        fs::write(config.join("haarness.json"), "{}").unwrap();
        let off = load_layers(&config, Some(&home), &workspace, true, &[]);
        assert!(off.errors.is_empty(), "{:?}", off.errors);
        let instructions = off.instructions.clone().unwrap();
        assert!(instructions.contains("PROJECT_AGENTS"));
        for absent in [
            "HOME_CLAUDE",
            "PROJECT_LOCAL",
            "WORKSPACE_CLAUDE",
            "CONTEXT",
        ] {
            assert!(!instructions.contains(absent), "{absent}");
        }
        assert_eq!(
            off.commands
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["deploy"]
        );
        assert!(off.agents.is_empty());

        // Instructions only.
        fs::write(
            config.join("haarness.json"),
            r#"{"compatibility":{"claude_code":{"instructions":true}}}"#,
        )
        .unwrap();
        let instructions = load_layers(&config, Some(&home), &workspace, true, &[])
            .instructions
            .unwrap();
        for expected in [
            "HOME_CLAUDE",
            "PROJECT_AGENTS",
            "PROJECT_LOCAL",
            "WORKSPACE_CLAUDE",
        ] {
            assert_eq!(
                instructions.matches(expected).count(),
                1,
                "{expected}\n{instructions}"
            );
        }
        assert!(
            !instructions.contains("PROJECT_CLAUDE_LOSES"),
            "AGENTS.md wins in its directory"
        );
        assert!(
            !instructions.contains("WORKSPACE_CONTEXT_LOSES"),
            ".claude/CLAUDE.md wins over CONTEXT.md"
        );
        assert!(
            instructions.find("HOME_CLAUDE").unwrap()
                < instructions.find("PROJECT_AGENTS").unwrap()
        );
        assert!(
            instructions.find("PROJECT_AGENTS").unwrap()
                < instructions.find("PROJECT_LOCAL").unwrap()
        );

        // Every category, with the master switch respected.
        fs::write(
            config.join("haarness.json"),
            r#"{"compatibility":{"claude_code":{"instructions":true,"agents":true,"skills":true,"commands":true}}}"#,
        )
        .unwrap();
        let on = load_layers(&config, Some(&home), &workspace, true, &[]);
        assert!(on.errors.is_empty(), "{:?}", on.errors);
        let command = |name: &str| {
            on.commands
                .iter()
                .find(|c| c.name == name)
                .map(|c| c.template.as_str())
        };
        assert_eq!(
            command("deploy"),
            Some("NATIVE_DEPLOY"),
            "native resources win"
        );
        assert_eq!(command("lint"), Some("CLAUDE_LINT"));
        assert_eq!(command("home-cmd"), Some("HOME_COMMAND"));
        assert!(
            on.commands
                .iter()
                .any(|c| c.name == "home-skill" && c.template.contains("HOME_SKILL"))
        );
        assert!(
            on.commands
                .iter()
                .any(|c| c.name == "project-skill" && c.template.contains("PROJECT_SKILL"))
        );
        let scout = on.agents.iter().find(|a| a.id == "scout").expect("scout");
        assert_eq!(scout.color.as_deref(), Some("error"));
        assert_eq!(scout.model, None, "inherit means no pin");
        assert_eq!(scout.tools.get("*"), Some(&false));
        assert_eq!(scout.tools.get("read"), Some(&true));
        assert_eq!(scout.tools.get("grep"), Some(&true));
        let fixer = on.agents.iter().find(|a| a.id == "fixer").expect("fixer");
        assert_eq!(fixer.color.as_deref(), Some("#123abc"));
        assert_eq!(
            fixer.model.as_deref(),
            Some("sonnet"),
            "aliases resolve at dispatch"
        );
        assert_eq!(fixer.tools.get("edit"), Some(&true));
        assert_eq!(fixer.tools.get("bash"), Some(&true));

        fs::write(
            config.join("haarness.json"),
            r#"{"compatibility":{"claude_code":{"enabled":false,"instructions":true,"agents":true,"skills":true,"commands":true}}}"#,
        )
        .unwrap();
        let disabled = load_layers(&config, Some(&home), &workspace, true, &[]);
        assert!(disabled.agents.is_empty());
        assert_eq!(disabled.commands.len(), 1);
        assert!(!disabled.instructions.unwrap().contains("HOME_CLAUDE"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn claude_code_frontmatter_loads_and_unparseable_definitions_are_reported() {
        let root = std::env::temp_dir().join(format!(
            "haarness-claude-frontmatter-{}",
            uuid::Uuid::now_v7()
        ));
        let config = root.join("config");
        let workspace = root.join("workspace");
        for directory in [
            config.clone(),
            workspace.join(".git"),
            workspace.join(".claude/commands"),
            workspace.join(".claude/agents"),
            workspace.join(".claude/skills/broken"),
        ] {
            fs::create_dir_all(directory).unwrap();
        }
        fs::write(
            config.join("haarness.json"),
            r#"{"compatibility":{"claude_code":{"agents":true,"skills":true,"commands":true}}}"#,
        )
        .unwrap();
        // Frontmatter Claude Code documents, which YAML rejects as written.
        fs::write(
            workspace.join(".claude/commands/export.md"),
            "---\ndescription: Export: one file\nargument-hint: [filename] [format]\nallowed-tools: Bash(git commit: *), Read\n---\nExport $1 as $2.",
        )
        .unwrap();
        fs::write(
            workspace.join(".claude/agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Reviews: diffs\ntools: Read, Bash(git log: *)\nmodel: claude-sonnet-5-5\n---\nReview the diff.",
        )
        .unwrap();
        let broken = "---\ndescription: Broken\nallowed-tools:\n  - Bash(git commit: *)\n---\nBody";
        let skipped = [
            ("command", workspace.join(".claude/commands/broken.md")),
            ("agent", workspace.join(".claude/agents/broken.md")),
            ("skill", workspace.join(".claude/skills/broken/SKILL.md")),
        ];
        for (_, path) in &skipped {
            fs::write(path, broken).unwrap();
        }
        let layers = load_layers(&config, None, &workspace, true, &[]);
        assert!(layers.errors.is_empty(), "{:?}", layers.errors);
        let export = layers
            .commands
            .iter()
            .find(|command| command.name == "export")
            .expect("export command");
        assert_eq!(export.description, "Export: one file");
        assert_eq!(export.argument_hint.as_deref(), Some("[filename] [format]"));
        assert_eq!(
            export.execution.allowed_tools,
            vec!["Bash(git commit: *)", "Read"]
        );
        let reviewer = layers
            .agents
            .iter()
            .find(|agent| agent.id == "reviewer")
            .expect("reviewer agent");
        assert_eq!(reviewer.description, "Reviews: diffs");
        assert_eq!(reviewer.model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(reviewer.tools.get("bash"), Some(&true));
        assert!(
            !layers
                .commands
                .iter()
                .any(|command| command.name == "broken")
        );
        assert!(!layers.agents.iter().any(|agent| agent.id == "broken"));
        for (kind, path) in skipped {
            let prefix = format!(
                "skipped {kind} {}: invalid definition frontmatter",
                path.display()
            );
            assert!(
                layers
                    .notices
                    .iter()
                    .any(|notice| notice.starts_with(&prefix)),
                "{prefix}\n{:?}",
                layers.notices
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn json_frontmatter_keeps_its_limits_and_code_frontmatter_is_skipped() {
        let root = std::env::temp_dir().join(format!(
            "haarness-frontmatter-languages-{}",
            uuid::Uuid::now_v7()
        ));
        let config = root.join("config");
        let workspace = root.join("workspace");
        for directory in [
            config.clone(),
            workspace.join(".git"),
            workspace.join(".haarness/agents"),
            workspace.join(".haarness/commands"),
            workspace.join(".haarness/skills/scripted"),
        ] {
            fs::create_dir_all(directory).unwrap();
        }
        fs::write(
            workspace.join(".haarness/agents/limited.md"),
            "---json\n{\"description\": \"Limited\", \"permission\": {\"bash\": \"deny\"}}\n---\nReview only.",
        )
        .unwrap();
        fs::write(
            workspace.join(".haarness/agents/yaml-limited.md"),
            "---yaml\ndescription: Yaml limited\npermission:\n  edit: deny\n---\nRead only.",
        )
        .unwrap();
        let skipped = [
            (
                "agent",
                workspace.join(".haarness/agents/scripted.md"),
                "---js\nmodule.exports = { permission: {} }\n---\nDo anything.",
                "JavaScript frontmatter is not supported",
            ),
            (
                "command",
                workspace.join(".haarness/commands/scripted.md"),
                "---coffee\ndescription: 'x'\n---\nRun it.",
                "frontmatter language \"coffee\" is not supported",
            ),
            (
                "skill",
                workspace.join(".haarness/skills/scripted/SKILL.md"),
                "---javascript\n({ name: 'scripted' })\n---\nSteps.",
                "JavaScript frontmatter is not supported",
            ),
        ];
        for (_, path, text, _) in &skipped {
            fs::write(path, text).unwrap();
        }
        let layers = load_layers(&config, None, &workspace, true, &[]);
        let limited = layers
            .agents
            .iter()
            .find(|agent| agent.id == "limited")
            .expect("the JSON agent loads");
        assert_eq!(limited.description, "Limited");
        assert_eq!(limited.prompt, "Review only.");
        assert!(
            limited
                .permissions
                .iter()
                .any(|rule| rule.pattern == "bash:*"
                    && rule.decision == haarness_core::PermissionDecision::Deny),
            "{:?}",
            limited.permissions
        );
        let yaml = layers
            .agents
            .iter()
            .find(|agent| agent.id == "yaml-limited")
            .expect("the ---yaml agent loads");
        assert_eq!(yaml.prompt, "Read only.");
        assert!(!yaml.permissions.is_empty());
        assert!(!layers.agents.iter().any(|agent| agent.id == "scripted"));
        assert!(
            !layers
                .commands
                .iter()
                .any(|command| command.name == "scripted")
        );
        for (kind, path, _, reason) in skipped {
            let expected = format!("skipped {kind} {}: {reason}", path.display());
            assert!(
                layers
                    .notices
                    .iter()
                    .any(|notice| notice.starts_with(&expected)),
                "{expected}\n{:?}",
                layers.notices
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn agent_frontmatter_and_json_entries_carry_presentation_limits_and_disable() {
        let root =
            std::env::temp_dir().join(format!("haarness-agent-fields-{}", uuid::Uuid::now_v7()));
        let config = root.join("config");
        let workspace = root.join("workspace");
        fs::create_dir_all(config.join("agents")).unwrap();
        fs::create_dir_all(workspace.join(".haarness/agents")).unwrap();
        fs::write(
            config.join("agents/quiet.md"),
            "---\ndescription: Quiet helper\nmode: subagent\nhidden: true\ncolor: Purple\ntemperature: 0.2\ntop_p: 0.9\nmaxSteps: 12\nvariant: fast\n---\nHelp quietly.",
        )
        .unwrap();
        fs::write(
            config.join("agents/retired.md"),
            "---\ndescription: Retired\n---\nOld prompt.",
        )
        .unwrap();
        fs::write(
            workspace.join(".haarness/agents/retired.md"),
            "---\ndescription: Retired\ndisable: true\n---\nStill has a body.",
        )
        .unwrap();
        fs::write(
            workspace.join("haarness.json"),
            serde_json::json!({
                "agent": {
                    "json-agent": {
                        "prompt": "From JSON", "hidden": true, "color": "warning",
                        "steps": 3, "temperature": 0.5, "top_p": 0.8, "variant": "deep",
                        "mode": "subagent"
                    },
                    "quiet": {"prompt": "Replaced", "disable": true}
                },
                "command": {
                    "forked": {"template": "Do $ARGUMENTS", "subtask": true},
                    "inline": {"template": "Do $ARGUMENTS"}
                }
            })
            .to_string(),
        )
        .unwrap();
        let layers = load_layers(&config, None, &workspace, true, &[]);
        assert!(layers.errors.is_empty(), "{:?}", layers.errors);
        assert!(
            !layers.agents.iter().any(|agent| agent.id == "retired"),
            "disable retires the agent from every layer"
        );
        assert!(
            !layers.agents.iter().any(|agent| agent.id == "quiet"),
            "a disabled JSON entry retires the markdown agent"
        );
        let json_agent = layers.agents.iter().find(|a| a.id == "json-agent").unwrap();
        assert!(json_agent.hidden);
        assert_eq!(json_agent.color.as_deref(), Some("warning"));
        assert_eq!(json_agent.steps, Some(3));
        assert_eq!(json_agent.temperature, Some(0.5));
        assert_eq!(json_agent.top_p, Some(0.8));
        assert_eq!(json_agent.variant.as_deref(), Some("deep"));
        let forked = layers.commands.iter().find(|c| c.name == "forked").unwrap();
        assert!(forked.execution.fork);
        assert!(
            !layers
                .commands
                .iter()
                .find(|c| c.name == "inline")
                .unwrap()
                .execution
                .fork
        );

        // Markdown fields alone.
        fs::remove_file(workspace.join("haarness.json")).unwrap();
        let layers = load_layers(&config, None, &workspace, true, &[]);
        let quiet = layers.agents.iter().find(|a| a.id == "quiet").unwrap();
        assert!(quiet.hidden);
        assert_eq!(quiet.color.as_deref(), Some("accent"));
        assert_eq!(quiet.temperature, Some(0.2));
        assert_eq!(quiet.top_p, Some(0.9));
        assert_eq!(quiet.steps, Some(12));
        assert_eq!(quiet.variant.as_deref(), Some("fast"));
        assert_eq!(agent_color("lime"), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remote_skill_indexes_are_cached_and_skill_paths_add_commands() {
        let root =
            std::env::temp_dir().join(format!("haarness-remote-skills-{}", uuid::Uuid::now_v7()));
        let config = root.join("config");
        let workspace = root.join("workspace");
        let home = root.join("home");
        let cache = root.join("data/remote-skills");
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(workspace.join("team-skills/deploy")).unwrap();
        fs::create_dir_all(home.join("skills/audit")).unwrap();
        fs::write(
            workspace.join("team-skills/deploy/SKILL.md"),
            "---\nname: deploy\ndescription: Deploy\n---\nTEAM_DEPLOY",
        )
        .unwrap();
        fs::write(
            home.join("skills/audit/SKILL.md"),
            "---\nname: audit\ndescription: Audit\n---\nHOME_AUDIT",
        )
        .unwrap();
        fs::write(
            workspace.join("haarness.json"),
            r#"{"skills":{"paths":["./team-skills", "~/skills", "./missing"]}}"#,
        )
        .unwrap();
        let layers = load_layers(&config, Some(&home), &workspace, true, &[]);
        assert!(layers.errors.is_empty(), "{:?}", layers.errors);
        assert!(
            layers
                .commands
                .iter()
                .any(|c| c.name == "deploy" && c.template == "TEAM_DEPLOY")
        );
        assert!(
            layers
                .commands
                .iter()
                .any(|c| c.name == "audit" && c.template == "HOME_AUDIT")
        );
        assert_eq!(
            configured_skill_paths(&layers.native, Some(&home), &workspace),
            vec![workspace.join("./team-skills"), home.join("skills")]
        );

        let base = serve(vec![
            (
                "/skills/index.json",
                r#"{"skills":[
                    {"name":"remote-a","description":"A","files":["SKILL.md","reference.md"]},
                    {"name":"remote-b","url":"/direct/remote-b.md"},
                    {"name":"../escape","files":["SKILL.md"]},
                    {"name":"broken","files":["../SKILL.md"]},
                    {"name":"sneaky","files":["SKILL.md","%2e%2e/%2E%2e/direct/remote-b.md"]},
                    {"name":"missing"}
                ]}"#,
            ),
            (
                "/skills/remote-a/SKILL.md",
                "---\nname: remote-a\ndescription: Remote A\n---\nREMOTE_A",
            ),
            ("/skills/remote-a/reference.md", "REFERENCE"),
            (
                "/skills/sneaky/SKILL.md",
                "---\nname: sneaky\ndescription: Sneaky\n---\nSNEAKY",
            ),
            (
                "/direct/remote-b.md",
                "---\nname: remote-b\ndescription: Remote B\n---\nREMOTE_B",
            ),
        ]);
        let host = pull_remote_skills(&cache, &format!("{base}/skills"))
            .unwrap()
            .expect("host root");
        assert!(host.starts_with(&cache));
        assert_eq!(
            fs::read_to_string(host.join("remote-a/SKILL.md")).unwrap(),
            "---\nname: remote-a\ndescription: Remote A\n---\nREMOTE_A"
        );
        assert_eq!(
            fs::read_to_string(host.join("remote-a/reference.md")).unwrap(),
            "REFERENCE"
        );
        assert!(host.join("remote-b/SKILL.md").is_file());
        assert!(!host.join("missing/SKILL.md").exists());
        assert!(!cache.join("escape").exists());
        assert!(
            !host.join("SKILL.md").exists(),
            "parent traversal is rejected"
        );
        // An escaped `..` is not fetched from outside the skill's folder.
        assert!(host.join("sneaky/SKILL.md").is_file());
        assert!(!host.join("sneaky/%2e%2e").exists());
        let mut commands = Vec::new();
        discover_skill_commands(&mut commands, &mut Vec::new(), &host, None);
        let mut names = commands.iter().map(|c| c.name.as_str()).collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(names, vec!["remote-a", "remote-b", "sneaky"]);
        // A trailing index.json and a trailing slash name the same index.
        assert_eq!(
            pull_remote_skills(&cache, &format!("{base}/skills/index.json")).unwrap(),
            Some(host.clone())
        );
        assert!(pull_remote_skills(&cache, "http://127.0.0.1:1/none/").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_config_opt_out_keeps_global_layers_and_skips_all_project_sources() {
        let root =
            std::env::temp_dir().join(format!("haarness-project-opt-out-{}", uuid::Uuid::now_v7()));
        let config = root.join("config");
        let project = root.join("project");
        let home = root.join("home");
        for dir in [
            config.join("commands"),
            project.join(".haarness/commands"),
            project.join(".claude"),
            home.clone(),
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(
            config.join("haarness.json"),
            r#"{"model":"echo/global","permission":{"bash":"deny"}}"#,
        )
        .unwrap();
        fs::write(config.join("commands/check.md"), "Global command").unwrap();
        fs::write(config.join("AGENTS.md"), "GLOBAL_INSTRUCTIONS").unwrap();
        fs::write(
            project.join("haarness.json"),
            r#"{"model":"echo/project","permission":{"bash":"allow"},"hooks":{"before_tool":[]}}"#,
        )
        .unwrap();
        fs::write(
            project.join(".haarness/commands/check.md"),
            "Project command",
        )
        .unwrap();
        fs::write(project.join("AGENTS.md"), "PROJECT_INSTRUCTIONS").unwrap();
        fs::write(
            project.join(".claude/settings.json"),
            r#"{"permissions":{"allow":["Write"]}}"#,
        )
        .unwrap();
        let enabled = load_layers(&config, Some(&home), &project, true, &[]);
        assert_eq!(enabled.model.as_deref(), Some("project"));
        assert!(
            enabled
                .instructions
                .unwrap()
                .contains("PROJECT_INSTRUCTIONS")
        );

        let disabled = load_layers(&config, Some(&home), &project, false, &[]);
        assert!(disabled.errors.is_empty());
        assert_eq!(disabled.model.as_deref(), Some("global"));
        assert_eq!(
            disabled
                .commands
                .iter()
                .find(|item| item.name == "check")
                .unwrap()
                .template,
            "Global command"
        );
        assert!(disabled.hook_sources.is_empty());
        assert_eq!(disabled.rules.len(), 1);
        assert_eq!(disabled.rules[0].decision, PermissionDecision::Deny);
        let instructions = disabled.instructions.unwrap();
        assert!(instructions.contains("GLOBAL_INSTRUCTIONS"));
        assert!(!instructions.contains("PROJECT_INSTRUCTIONS"));
        fs::write(project.join("haarness.json"), "invalid project JSON").unwrap();
        assert!(
            load_layers(&config, Some(&home), &project, false, &[])
                .errors
                .is_empty()
        );
        assert!(
            !load_layers(&config, Some(&home), &project, true, &[])
                .errors
                .is_empty()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn claude_code_home_and_project_files_are_never_discovered() {
        let root =
            std::env::temp_dir().join(format!("haarness-no-claude-{}", uuid::Uuid::now_v7()));
        let config = root.join("config");
        let home = root.join("home");
        let project = root.join("project");
        for directory in [
            config.join("commands"),
            home.join(".claude/commands"),
            home.join(".claude/agents"),
            home.join(".claude/skills/ignored"),
            project.join(".claude/commands"),
            project.join(".claude/agents"),
            project.join(".claude/skills/ignored"),
        ] {
            fs::create_dir_all(directory).unwrap();
        }
        fs::write(config.join("haarness.json"), r#"{"model":"echo/haarness"}"#).unwrap();
        fs::write(config.join("commands/visible.md"), "VISIBLE").unwrap();
        for base in [home.join(".claude"), project.join(".claude")] {
            fs::write(base.join("settings.json"), r#"{"model":"echo/claude","hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"false"}]}]},"mcpServers":{"claude":{"command":"false"}},"permissions":{"allow":["Bash(*)"]}}"#).unwrap();
            fs::write(base.join("settings.local.json"), "invalid json").unwrap();
            fs::write(base.join("commands/ignored.md"), "CLAUDE_COMMAND").unwrap();
            fs::write(
                base.join("agents/ignored.md"),
                "---\ndescription: Claude agent\n---\nCLAUDE_AGENT",
            )
            .unwrap();
            fs::write(
                base.join("skills/ignored/SKILL.md"),
                "---\nname: ignored\ndescription: Claude skill\n---\nCLAUDE_SKILL",
            )
            .unwrap();
            fs::write(base.join("CLAUDE.md"), "CLAUDE_INSTRUCTIONS").unwrap();
        }
        fs::write(project.join("CLAUDE.md"), "CLAUDE_ROOT_INSTRUCTIONS").unwrap();
        fs::write(
            project.join(".mcp.json"),
            r#"{"mcpServers":{"claude":{"command":"false"}}}"#,
        )
        .unwrap();
        let layers = load_layers(&config, Some(&home), &project, true, &[]);
        assert!(layers.errors.is_empty(), "{:?}", layers.errors);
        assert_eq!(layers.model.as_deref(), Some("haarness"));
        assert_eq!(
            layers
                .commands
                .iter()
                .map(|command| command.name.as_str())
                .collect::<Vec<_>>(),
            vec!["visible"]
        );
        assert!(layers.agents.is_empty());
        assert!(layers.hooks.is_empty());
        assert!(layers.hook_sources.is_empty());
        assert!(layers.mcp.is_empty());
        assert!(layers.rules.is_empty());
        assert!(layers.instructions.is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_layers_override_global_resources_and_permissions() {
        let root = std::env::temp_dir().join(format!("haarness-scope-{}", uuid::Uuid::now_v7()));
        let config = root.join("config");
        let project = root.join("project");
        let child = project.join("child");
        for dir in [
            config.join("agents"),
            config.join("commands"),
            project.join(".git"),
            project.join(".haarness/agents"),
            child.join(".haarness/commands"),
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(config.join("haarness.json"), r#"{"model":"echo/global","permission":{"bash":"deny"},"providers":{"fixture":{"enabled":false}}}"#).unwrap();
        fs::write(
            project.join(".haarness/config.toml"),
            "[defaults]\nmodel=\"project\"\n[permissions]\nbash=\"ask\"\n",
        )
        .unwrap();
        fs::write(child.join(".haarness/haarness.jsonc"), r#"{// project override
            "model":"echo/child", "permission":{"bash":"allow"}, "providers":{"fixture":{"enabled":true}},
        }"#).unwrap();
        fs::write(
            config.join("agents/reviewer.md"),
            "---\ndescription: global\n---\nGlobal instructions",
        )
        .unwrap();
        fs::write(
            project.join(".haarness/agents/reviewer.md"),
            "---\ndescription: project\n---\nProject instructions",
        )
        .unwrap();
        fs::write(config.join("commands/review.md"), "Global command").unwrap();
        fs::write(child.join(".haarness/commands/review.md"), "Child command").unwrap();
        let layers = load_layers(&config, None, &child, true, &[]);
        assert!(layers.errors.is_empty(), "{:?}", layers.errors);
        assert_eq!(
            layers.native.defaults.unwrap().model.as_deref(),
            Some("child")
        );
        assert!(layers.native.providers["fixture"].enabled);
        assert_eq!(
            layers.rules.last().unwrap().decision,
            PermissionDecision::Allow
        );
        assert_eq!(
            layers
                .agents
                .iter()
                .find(|agent| agent.id == "reviewer")
                .unwrap()
                .description,
            "project"
        );
        assert_eq!(
            layers
                .commands
                .iter()
                .find(|command| command.name == "review")
                .unwrap()
                .template,
            "Child command"
        );
        fs::remove_file(child.join(".haarness/haarness.jsonc")).unwrap();
        let layers = load_layers(&config, None, &child, true, &[]);
        assert_eq!(
            layers.native.defaults.unwrap().model.as_deref(),
            Some("project")
        );
        assert_eq!(
            layers.rules.last().unwrap().decision,
            PermissionDecision::Ask
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scoped_json_writes_keep_sibling_comments_and_mcp_metadata() {
        let root =
            std::env::temp_dir().join(format!("haarness-json-write-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("haarness.jsonc");
        fs::write(
            &path,
            "{\n  // personal setting\n  \"model\": \"echo/echo-1\",\n}\n",
        )
        .unwrap();
        write_entry(&path, "mcp", "fixture", Some(serde_json::json!({"command":"true", "args":["a b"],"timeoutMs":12000,"enabled":false}))).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("// personal setting"));
        let value = read_json(&path).unwrap().unwrap();
        let entry = mcp_entry(&value["mcp"]["fixture"]).unwrap();
        assert_eq!(entry.timeout_ms, Some(12000));
        assert_eq!(entry.args, ["a b"]);
        assert!(!entry.enabled);
        write_entry(&path, "mcp", "fixture", None).unwrap();
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("// personal setting")
        );
        // Nothing but the configuration is left in the folder (a project's
        // root): no lock or temporary file.
        let names: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["haarness.jsonc"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_json_writes_keep_every_entry() {
        let root =
            std::env::temp_dir().join(format!("haarness-json-race-{}", uuid::Uuid::now_v7()));
        let path = root.join("haarness.json");
        let writers: Vec<_> = (0..8)
            .map(|index| {
                let path = path.clone();
                std::thread::spawn(move || {
                    write_entry(
                        &path,
                        "mcp",
                        &format!("server{index}"),
                        Some(serde_json::json!({"command":"true"})),
                    )
                    .unwrap();
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        let value = read_json(&path).unwrap().unwrap();
        assert_eq!(value["mcp"].as_object().unwrap().len(), 8);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn permission_block_compiles_in_declaration_order() {
        let mut rules = Vec::new();
        compile_permission(
            &mut rules,
            &serde_json::json!({
                "bash": {"*": "ask", "git status *": "allow"},
                "edit": "allow",
            }),
            Path::new("haarness.json"),
            "permission",
        );
        let patterns: Vec<&str> = rules.iter().map(|rule| rule.pattern.as_str()).collect();
        assert_eq!(patterns, vec!["bash:*", "bash:git status *", "edit:*"]);
        assert_eq!(rules[1].decision, PermissionDecision::Allow);
    }

    #[test]
    fn a_permission_action_reads_in_any_letter_case_and_another_value_is_logged() {
        #[derive(Clone, Default)]
        struct Log(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Log {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let log = Log::default();
        let writer = log.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish();
        let file = Path::new("/project/haarness.json");
        let mut rules = Vec::new();
        tracing::subscriber::with_default(subscriber, || {
            for _ in 0..2 {
                rules.clear();
                compile_permission(
                    &mut rules,
                    &serde_json::json!({
                        "bash": {"rm *": "Deny", "git push *": "ASK", "git status *": "dney"},
                        "edit": "Allow",
                        "webfetch": true,
                    }),
                    file,
                    "permission",
                );
            }
        });
        let compiled: Vec<(&str, PermissionDecision)> = rules
            .iter()
            .map(|rule| (rule.pattern.as_str(), rule.decision))
            .collect();
        assert_eq!(
            compiled,
            vec![
                ("bash:rm *", PermissionDecision::Deny),
                ("bash:git push *", PermissionDecision::Ask),
                ("edit:*", PermissionDecision::Allow),
            ]
        );
        let text = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
        // One line for each value nobody can read, however often it loads.
        assert_eq!(
            text.matches("config: ignored \"permission.bash.git status *\" in /project/haarness.json: \"dney\" is not allow, ask or deny").count(),
            1,
            "{text}"
        );
        assert_eq!(
            text.matches("config: ignored \"permission.webfetch\" in /project/haarness.json: true is not allow, ask or deny").count(),
            1,
            "{text}"
        );
        assert_eq!(text.lines().count(), 2, "{text}");
    }

    #[test]
    fn a_migrated_config_keeps_its_permission_rules_in_the_order_written() {
        let root =
            std::env::temp_dir().join(format!("haarness-migrate-order-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        let toml = root.join("config.toml");
        fs::write(
            &toml,
            "[permissions]\nedit = \"ask\"\n\"*\" = \"allow\"\n[permissions.bash]\n\"git push*\" = \"allow\"\n\"git push --force*\" = \"deny\"\n",
        )
        .unwrap();
        let mut notices = Vec::new();
        migrate_toml(&toml, &mut notices).unwrap();
        let migrated: Value =
            serde_json::from_str(&fs::read_to_string(root.join("haarness.json")).unwrap()).unwrap();
        let mut rules = Vec::new();
        compile_permission(
            &mut rules,
            &migrated["permission"],
            &root.join("haarness.json"),
            "permission",
        );
        let patterns: Vec<&str> = rules.iter().map(|rule| rule.pattern.as_str()).collect();
        assert_eq!(
            patterns,
            vec!["edit:*", "*", "bash:git push*", "bash:git push --force*"]
        );
        assert!(root.join("config.toml.migrated").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn goal_harness_defaults_load_from_jsonc() {
        let root =
            std::env::temp_dir().join(format!("haarness-goal-config-{}", uuid::Uuid::now_v7()));
        let config = root.join("config");
        let workspace = root.join("workspace");
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        fs::write(
            workspace.join("haarness.jsonc"),
            r#"{
                // Match Haarness' public configuration shape.
                "experimental": {
                    "goal_harness": { "tokenBudget": 12000, "maxTurns": 7 }
                }
            }"#,
        )
        .unwrap();

        let layers = load_layers(&config, None, &workspace, true, &[]);
        assert_eq!(layers.goal_token_budget, Some(12_000));
        assert_eq!(layers.goal_max_turns, Some(7));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_existing_json_is_reported_for_last_known_good_reload() {
        let root =
            std::env::temp_dir().join(format!("haarness-invalid-config-{}", uuid::Uuid::now_v7()));
        let config = root.join("config");
        let workspace = root.join("workspace");
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("haarness.jsonc"), "{ invalid").unwrap();

        let layers = load_layers(&config, None, &workspace, true, &[]);
        assert_eq!(layers.errors.len(), 1);
        assert!(layers.errors[0].contains("haarness.jsonc"));
        assert!(layers.errors[0].contains("invalid JSON"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn instruction_layers_label_sources_and_trust_only_global_external_imports() {
        let root = std::env::temp_dir().join(format!(
            "haarness-instruction-layers-{}",
            uuid::Uuid::now_v7()
        ));
        let config = root.join("config");
        let workspace = root.join("workspace");
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        fs::write(config.join("global-extra.md"), "portable global detail").unwrap();
        fs::write(config.join("AGENTS.md"), "global rule\n@global-extra.md").unwrap();
        fs::write(
            workspace.join("AGENTS.md"),
            format!(
                "project rule\n@{}",
                config.join("global-extra.md").display()
            ),
        )
        .unwrap();

        let instructions = load_layers(&config, None, &workspace, true, &[])
            .instructions
            .expect("instruction layers");
        assert!(instructions.contains(&format!(
            "<instructions source=\"{}\">",
            config.join("AGENTS.md").display()
        )));
        assert!(instructions.contains(&format!(
            "<instructions source=\"{}\">",
            workspace.join("AGENTS.md").display()
        )));
        assert_eq!(
            instructions.matches("portable global detail").count(),
            1,
            "trusted global imports expand, while project imports cannot escape the workspace"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn env_expansion_handles_fallbacks() {
        // HOME is reliably set in test environments.
        let home = std::env::var("HOME").unwrap_or_default();
        assert_eq!(expand_env("root: ${HOME}"), format!("root: {home}"));
        assert_eq!(expand_env("${MISSING_VAR_HAARNESS:-fallback}"), "fallback");
        assert_eq!(expand_env("${MISSING_VAR_HAARNESS}"), "");
        assert_eq!(expand_env("plain"), "plain");
    }

    /// A load resolves what the MCP client uses as given and leaves
    /// environment and header references to the client, which resolves them
    /// once when it connects: resolved here too, a variable whose value holds
    /// `${OTHER}` would have that read as well and sent.
    #[test]
    fn loaded_mcp_entries_leave_environment_and_header_references_to_the_connection() {
        let home = std::env::var("HOME").unwrap_or_default();
        let local = serde_json::json!({
            "command": "${HOME}/bin/server",
            "args": ["--root", "${HOME}"],
            "env": {"TOKEN": "${HAARNESS_FIXTURE_UNSET_TOKEN}", "MIXED": "a-${HOME}"}
        });
        let loaded = loaded_mcp_entry(&local, None).unwrap();
        assert_eq!(loaded.command, Some(format!("{home}/bin/server")));
        assert_eq!(loaded.args, ["--root", home.as_str()]);
        assert_eq!(loaded.env["TOKEN"], "${HAARNESS_FIXTURE_UNSET_TOKEN}");
        assert_eq!(loaded.env["MIXED"], "a-${HOME}");
        let remote = loaded_mcp_entry(
            &serde_json::json!({
                "url": "https://example.test/${HAARNESS_FIXTURE_UNSET_REGION:-eu}/mcp",
                "headers": {"Authorization": "Bearer ${HAARNESS_FIXTURE_UNSET_TOKEN}"}
            }),
            None,
        )
        .unwrap();
        assert_eq!(remote.url.as_deref(), Some("https://example.test/eu/mcp"));
        assert_eq!(
            remote.headers["Authorization"],
            "Bearer ${HAARNESS_FIXTURE_UNSET_TOKEN}"
        );
        // As written, for showing, editing and writing back.
        let written = mcp_entry(&local).unwrap();
        assert_eq!(written.command.as_deref(), Some("${HOME}/bin/server"));
        assert_eq!(written.args, ["--root", "${HOME}"]);
        assert_eq!(written.env["TOKEN"], "${HAARNESS_FIXTURE_UNSET_TOKEN}");
    }

    #[test]
    fn mcp_compatibility_preserves_local_environment_and_remote_headers() {
        let local = mcp_entry(&serde_json::json!({
            "type": "local",
            "command": ["server", "--stdio"],
            "environment": {"TOKEN": "fixture", "MODE": "test"}
        }))
        .unwrap();
        assert_eq!(local.command.as_deref(), Some("server"));
        assert_eq!(local.args, vec!["--stdio"]);
        assert_eq!(local.env.get("TOKEN").map(String::as_str), Some("fixture"));

        let remote = mcp_entry(&serde_json::json!({
            "type": "remote",
            "url": "https://example.test/mcp",
            "headers": {"Authorization": "Bearer fixture"}
        }))
        .unwrap();
        assert_eq!(
            remote.headers.get("Authorization").map(String::as_str),
            Some("Bearer fixture")
        );
        assert!(remote.env.is_empty());
    }

    #[test]
    fn frontmatter_splits_and_body_survives() {
        let (front, body) = haarness_core::definitions::markdown(
            "---\ndescription: Review code\nagent: review\n---\nDo a review of $ARGUMENTS",
        )
        .unwrap();
        assert_eq!(front.get("description").unwrap(), "Review code");
        assert_eq!(front.get("agent").unwrap(), "review");
        assert!(body.contains("Do a review of $ARGUMENTS"));
        let (empty, body) = haarness_core::definitions::markdown("no frontmatter here").unwrap();
        assert!(empty.is_empty());
        assert_eq!(body, "no frontmatter here");
    }

    #[test]
    fn plugin_commands_and_skills_use_claude_namespaces() {
        let root =
            std::env::temp_dir().join(format!("haarness-plugin-config-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        fs::create_dir_all(root.join("commands")).unwrap();
        fs::create_dir_all(root.join("skills/design")).unwrap();
        fs::write(
            root.join(".claude-plugin/plugin.json"),
            r#"{"name":"demo"}"#,
        )
        .unwrap();
        fs::write(
            root.join("commands/ship.md"),
            "---\ndescription: Ship code\n---\nShip $ARGUMENTS",
        )
        .unwrap();
        fs::write(
            root.join("skills/design/SKILL.md"),
            "---\nname: design\ndescription: Design well\n---\nUse good hierarchy.",
        )
        .unwrap();
        let workspace = root.join("workspace");
        let layers = load_layers(
            &root.join("config"),
            None,
            &workspace,
            true,
            std::slice::from_ref(&root),
        );

        assert!(layers.commands.iter().any(|command| {
            command.name == "demo:ship" && command.plugin.as_deref() == Some("demo")
        }));
        assert!(layers.commands.iter().any(|command| {
            command.name == "demo:design" && command.template.contains("Use good hierarchy")
        }));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn plugin_agents_are_recursive_and_scoped_to_the_manifest_name() {
        let root =
            std::env::temp_dir().join(format!("haarness-plugin-agent-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        fs::create_dir_all(root.join("agents/review")).unwrap();
        fs::write(
            root.join(".claude-plugin/plugin.json"),
            r#"{"name":"review-kit"}"#,
        )
        .unwrap();
        fs::write(
            root.join("agents/router.md"),
            "---\nname: custom-router\ndescription: Route work\n---\nRoute requests.",
        )
        .unwrap();
        fs::write(
            root.join("agents/review/security.md"),
            "---\nname: security\ndescription: Review security\n---\nReview carefully.",
        )
        .unwrap();

        let layers = load_layers(
            &root.join("config"),
            None,
            &root.join("workspace"),
            true,
            std::slice::from_ref(&root),
        );
        assert!(
            layers
                .agents
                .iter()
                .any(|agent| agent.id == "review-kit:custom-router")
        );
        assert!(
            layers
                .agents
                .iter()
                .any(|agent| agent.id == "review-kit:review:security")
        );
        assert!(!layers.agents.iter().any(|agent| agent.id == "security"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn plugin_manifest_mcp_servers_expand_the_claude_plugin_context() {
        let root =
            std::env::temp_dir().join(format!("haarness-plugin-mcp-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(
            root.join(".claude-plugin/plugin.json"),
            serde_json::json!({
                "name": "mcp-kit",
                "mcpServers": {
                    "inline": {
                        "type": "stdio",
                        "command": ["${CLAUDE_PLUGIN_ROOT}/bin/server", "--inline"],
                        "args": ["${CLAUDE_PLUGIN_ROOT}/fixture"],
                        "cwd": "${CLAUDE_PLUGIN_ROOT}",
                        "env": {"PLUGIN_HOME": "${CLAUDE_PLUGIN_ROOT}"},
                        "timeout": 12
                    },
                    "remote": {
                        "type": "http",
                        "url": "https://example.test/mcp",
                        "headers": {"X-Plugin": "${CLAUDE_PLUGIN_ROOT}"},
                        "headersHelper": "sh ${CLAUDE_PLUGIN_ROOT}/scripts/headers.sh"
                    }
                }
            })
            .to_string(),
        )
        .unwrap();

        let layers = load_layers(
            &root.join("config"),
            None,
            &root.join("workspace"),
            true,
            std::slice::from_ref(&root),
        );
        let rendered = root.display().to_string();
        let inline = layers.mcp.get("inline").unwrap();
        let command = format!("{rendered}/bin/server");
        assert_eq!(inline.command.as_deref(), Some(command.as_str()));
        assert_eq!(
            inline.args,
            vec!["--inline".to_owned(), format!("{rendered}/fixture")]
        );
        assert_eq!(inline.cwd.as_deref(), Some(rendered.as_str()));
        assert_eq!(inline.env.get("PLUGIN_HOME"), Some(&rendered));
        assert_eq!(inline.env.get("CLAUDE_PLUGIN_ROOT"), Some(&rendered));
        assert_eq!(inline.timeout_ms, Some(12_000));

        let remote = layers.mcp.get("remote").unwrap();
        assert_eq!(remote.transport, "streamable_http");
        assert_eq!(remote.headers.get("X-Plugin"), Some(&rendered));
        // The helper's shell reads the plugin root from its environment.
        assert_eq!(
            remote.headers_helper.as_deref(),
            Some("sh ${CLAUDE_PLUGIN_ROOT}/scripts/headers.sh")
        );
        assert_eq!(remote.env.get("CLAUDE_PLUGIN_ROOT"), Some(&rendered));

        let _ = fs::remove_dir_all(root);
    }

    /// A headers helper is shell text, so a configuration load leaves its
    /// `${VAR}` references for the helper's shell: pasted in, a value holding
    /// `$(…)` ran as a command.
    #[test]
    fn a_headers_helper_keeps_its_references_as_written() {
        let root =
            std::env::temp_dir().join(format!("haarness-helper-text-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(root.join("config")).unwrap();
        fs::write(
            root.join("config/haarness.json"),
            r#"{"mcp": {"remote": {"type": "http", "url": "https://example.test/mcp",
                "headersHelper": "get-token --home \"${HOME}\" --scope ${HAARNESS_FIXTURE_SCOPE:-all}"}}}"#,
        )
        .unwrap();
        let layers = load_layers(
            &root.join("config"),
            None,
            &root.join("workspace"),
            true,
            &[],
        );
        assert_eq!(
            layers.mcp["remote"].headers_helper.as_deref(),
            Some(r#"get-token --home "${HOME}" --scope ${HAARNESS_FIXTURE_SCOPE:-all}"#)
        );
        let _ = fs::remove_dir_all(root);
    }

    /// opencode's `timeout` is milliseconds, a Claude Code or plugin entry's
    /// seconds, and Haarness's own `timeoutMs` milliseconds.
    #[test]
    fn an_mcp_timeout_is_read_in_the_unit_its_entry_writes() {
        for (entry, expected) in [
            (
                serde_json::json!({"type": "local", "command": ["server"], "timeout": 9000}),
                9_000,
            ),
            (
                serde_json::json!({"type": "remote", "url": "https://example.test/mcp", "timeout": 5000}),
                5_000,
            ),
            (
                serde_json::json!({"type": "stdio", "command": "server", "timeout": 12}),
                12_000,
            ),
            (
                serde_json::json!({"type": "http", "url": "https://example.test/mcp", "timeout": 12}),
                12_000,
            ),
            (
                serde_json::json!({"transport": "stdio", "command": "server", "timeoutMs": 7000}),
                7_000,
            ),
        ] {
            assert_eq!(
                mcp_entry(&entry).unwrap().timeout_ms,
                Some(expected),
                "{entry}"
            );
        }
    }

    #[test]
    fn an_opencode_entry_gives_its_oauth_client_and_scopes() {
        let entry = mcp_entry(
            &serde_json::json!({"type": "remote", "url": "https://example.test/mcp",
            "oauth": {"clientId": "team-client", "scope": "read write"}}),
        )
        .unwrap();
        assert_eq!(entry.oauth, None);
        assert_eq!(entry.oauth_client_id.as_deref(), Some("team-client"));
        assert_eq!(entry.oauth_scopes, ["read", "write"]);
        let entry = mcp_entry(&serde_json::json!({"type": "remote", "url": "https://example.test/mcp", "oauth": false}))
            .unwrap();
        assert_eq!(entry.oauth, Some(false));
    }
}
