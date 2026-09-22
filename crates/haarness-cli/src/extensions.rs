//! Explicit extension trust and installation; runtime execution lives in its adapter crate.
use super::Paths;
use anyhow::{Context as _, Result, bail};
use clap::{Args, Subcommand};
use haarness_extensions::{Config, Entry, Extensions};
use std::{
    io::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Args)]
pub struct ExtensionArgs {
    /// Manage extensions for this project instead of all projects.
    #[arg(long, global = true)]
    project: bool,
    #[command(subcommand)]
    command: ExtensionCommand,
}
#[derive(Subcommand)]
enum ExtensionCommand {
    /// Create a typed, dependency-free starter extension.
    Init { path: PathBuf },
    /// Trust and install a local file/package or npm:package[@version]. Executes its initializer.
    Install {
        source: String,
        #[arg(long, default_value = "{}")]
        options: String,
    },
    /// Show configured sources and the global enable switch without running code.
    List,
    /// Enable all extensions, or one installed extension by name or list index.
    Enable { target: Option<String> },
    /// Disable all extensions, or one installed extension by name or list index.
    Disable { target: Option<String> },
    /// Unregister an extension by name or list index; preserves source files.
    Remove { target: String },
    /// Load enabled extensions and validate their API, hooks, and tool schemas.
    Check {
        /// Show extension console output while validating.
        #[arg(long)]
        verbose: bool,
    },
}

pub fn config_path(paths: &Paths) -> PathBuf {
    paths
        .config
        .parent()
        .unwrap_or(Path::new("."))
        .join("extensions.json")
}

pub fn scoped_config_path(paths: &Paths, workspace: &Path, project: bool) -> PathBuf {
    if project {
        workspace.join(".haarness/extensions.json")
    } else {
        config_path(paths)
    }
}

pub fn scoped_config(paths: &Paths, workspace: &Path, project: bool) -> Result<Config> {
    let path = scoped_config_path(paths, workspace, project);
    let mut config = Config::read(&path)?;
    if project && !path.exists() {
        // An unconfigured project inherits discovery enablement and timeout.
        // Keep entries scoped: managing this project must never rewrite global entries.
        let inherited = sources(paths, workspace, true)?;
        config.enabled = inherited.enabled;
        config.timeout_ms = inherited.timeout_ms;
        config.init_timeout_ms = inherited.init_timeout_ms;
        config.hook_timeout_ms = inherited.hook_timeout_ms;
        config.tool_timeout_ms = inherited.tool_timeout_ms;
        config.payload_max_bytes = inherited.payload_max_bytes;
    }
    Ok(config)
}

pub fn sources(paths: &Paths, workspace: &Path, trust: bool) -> Result<Config> {
    let global_path = config_path(paths);
    let mut layers = vec![(global_path, paths.config_dir().join("extensions"))];
    if trust {
        for directory in crate::layout::project_directories(workspace) {
            layers.push((
                directory.join(".haarness/extensions.json"),
                directory.join(".haarness/extensions"),
            ));
        }
    }
    let mut config = Config::default();
    let mut roots = Vec::new();
    let mut explicit_switch = false;
    for (path, root) in layers {
        let mut layer = Config::read(&path)?;
        explicit_switch |= path.exists();
        if !path.exists() {
            layer.enabled = config.enabled;
        }
        config.enabled |= layer.enabled;
        if path.exists() {
            // Absent fields inherit; changing one deadline must not reset the others.
            let fields: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
            if fields.get("timeout_ms").is_some() { config.timeout_ms = layer.timeout_ms; }
            if fields.get("init_timeout_ms").is_some() { config.init_timeout_ms = layer.init_timeout_ms; }
            if fields.get("hook_timeout_ms").is_some() { config.hook_timeout_ms = layer.hook_timeout_ms; }
            if fields.get("tool_timeout_ms").is_some() { config.tool_timeout_ms = layer.tool_timeout_ms; }
            if fields.get("payload_max_bytes").is_some() { config.payload_max_bytes = layer.payload_max_bytes; }
        }
        if layer.enabled {
            roots.push(root);
        }
        for mut entry in layer.entries {
            if entry.source.is_relative() {
                entry.source = path
                    .parent()
                    .context("extension config parent")?
                    .join(&entry.source);
            }
            if let Ok(source) = entry.source.canonicalize() {
                entry.source = source;
            }
            entry.enabled &= layer.enabled;
            config.entries.retain(|prior| prior.source != entry.source);
            config.entries.push(entry);
        }
    }
    // Haarness `extensions` and `plugin` declarations join the same
    // registration; an `extensions.json` entry for the same source (for
    // example `enabled: false`) wins. Declarations enable the extension
    // system unless an `extensions.json` switch exists to say otherwise.
    let declared = crate::config_specs::declarations(paths.config_dir(), workspace, trust);
    for spec in &declared.extensions {
        if let Ok(crate::config_specs::Resolution::Entry(entry)) =
            crate::config_specs::resolve_extension(spec, &paths.data)
            && !config.entries.iter().any(|prior| prior.source == entry.source)
        {
            config.enabled |= !explicit_switch;
            config.entries.push(entry);
        }
    }
    if !config.enabled {
        return Ok(config);
    }
    roots.extend(
        super::plugins::all_enabled_roots(
            &paths.data,
            paths.config_dir(),
            workspace,
            trust,
        )
        .into_iter()
        .map(|root| root.join("extensions")),
    );
    let mut discovered = Vec::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(root)? {
            let path = entry?.path();
            if path.is_file()
                && matches!(
                    path.extension().and_then(|ext| ext.to_str()),
                    Some("ts" | "js" | "mjs" | "mts")
                )
                && !path.to_string_lossy().ends_with(".d.ts")
                && !path.to_string_lossy().ends_with(".d.mts")
            {
                discovered.push(path.canonicalize()?);
            }
        }
    }
    discovered.sort();
    for source in discovered {
        if !config.entries.iter().any(|entry| entry.source == source) {
            config.entries.push(Entry {
                name: None,
                source,
                options: serde_json::json!({}),
                enabled: true,
            });
        }
    }
    Ok(config)
}

pub async fn load(paths: &Paths, workspace: &Path, trust: bool) -> Result<Arc<Extensions>> {
    Extensions::load(&sources(paths, workspace, trust)?, workspace).await
}

/// The effective configuration delivered to extension `config` hooks: the
/// resolved defaults, permission rules, resource names and execution limits
/// of one runtime generation. Credentials never appear here.
#[must_use]
pub fn config_summary(
    workspace: &Path,
    layers: &crate::jsonconfig::JsonLayers,
    runtime: &haarness_core::RuntimeConfig,
    extensions: &Extensions,
) -> serde_json::Value {
    serde_json::json!({
        "workspace": workspace,
        "provider": runtime.default_provider,
        "model": runtime.default_model,
        "small_model": runtime.small_model,
        "default_agent": runtime.default_agent,
        "reasoning_effort": runtime.default_reasoning_effort,
        "ultracode": runtime.default_ultracode,
        "permission": runtime.base_rules,
        "mcp": layers.mcp.keys().collect::<Vec<_>>(),
        "agents": layers.agents.iter().map(|agent| agent.id.as_str()).collect::<Vec<_>>(),
        "commands": layers.commands.iter().map(|command| command.name.as_str()).collect::<Vec<_>>(),
        "hooks": layers.hooks.len(),
        "extensions": extensions.names(),
        "execution": runtime.execution,
        "tool_availability": runtime.tool_availability,
    })
}

fn save(path: &Path, config: &Config) -> Result<()> {
    std::fs::create_dir_all(path.parent().context("extension configuration parent")?)?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::now_v7()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(&serde_json::to_vec_pretty(config)?)?;
    file.sync_all()?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

const STARTER: &str = r#"import type { Extension } from "./api";

export default {
  apiVersion: 1,
  name: "hello",
  tools: {
    greet: {
      description: "Greet someone by name",
      parameters: {
        type: "object",
        properties: { name: { type: "string" } },
        required: ["name"],
        additionalProperties: false,
      },
      execute(args: { name: string }) {
        return `Hello, ${args.name}!`;
      },
    },
  },
  hooks: {
    chat() { return { context: "Use the greeting tool when the user asks for a greeting." }; },
  },
} satisfies Extension;
"#;

pub async fn run(paths: &Paths, workspace: &Path, trust: bool, args: ExtensionArgs) -> Result<()> {
    let _lock = haarness_profiles::configuration_lock(&paths.data)?;
    let path = scoped_config_path(paths, workspace, args.project);
    let mut config = scoped_config(paths, workspace, args.project)?;
    for entry in &mut config.entries {
        if entry.source.is_relative() {
            entry.source = path
                .parent()
                .context("extension config parent")?
                .join(&entry.source);
        }
    }
    let enable = matches!(args.command, ExtensionCommand::Enable { .. });
    match args.command {
        ExtensionCommand::Init { path } => {
            std::fs::create_dir(&path).context("choose a new directory for the extension")?;
            std::fs::write(path.join("api.d.ts"), haarness_extensions::TYPES)?;
            std::fs::write(path.join("index.ts"), STARTER)?;
            std::fs::write(
                path.join("package.json"),
                "{\"name\":\"haarness-extension-hello\",\"version\":\"0.1.0\",\"type\":\"module\",\"haarness\":{\"extension\":\"index.ts\"}}\n",
            )?;
            println!(
                "Created {}. Edit index.ts, then run: haarness extension install {}",
                path.display(),
                path.display()
            );
            return Ok(());
        }
        ExtensionCommand::List => {
            println!(
                "Extensions: {}\nConfig: {}",
                if config.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                path.display()
            );
            for (index, entry) in config.entries.iter().enumerate() {
                println!(
                    "{}\t{}\t{}\t{}",
                    index + 1,
                    entry.name.as_deref().unwrap_or("(unchecked)"),
                    if entry.enabled { "enabled" } else { "disabled" },
                    entry.source.display()
                );
            }
            println!(
                "Use extension check to inspect loaded tools, including discovered plugin/project extensions."
            );
            return Ok(());
        }
        ExtensionCommand::Check { verbose } => {
            let mut config = sources(paths, workspace, trust)?;
            config.log_stderr = verbose;
            let extensions = Extensions::load(&config, workspace).await?;
            let tools = extensions.wrap_tools(Arc::new(haarness_core::EmptyToolRegistry)).await?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"extensions":extensions.names(),"tools":tools.list()})
                )?
            );
            return Ok(());
        }
        ExtensionCommand::Install { source, options } => {
            let options = serde_json::from_str(&options).context("--options must be JSON")?;
            let source = if let Some(spec) = source.strip_prefix("npm:") {
                install_npm(paths, spec).await?
            } else {
                haarness_extensions::entrypoint(Path::new(&source))?
            };
            let entry = Entry {
                name: None,
                source,
                options,
                enabled: true,
            };
            config.entries.retain(|prior| prior.source != entry.source);
            config.entries.push(entry);
            config.enabled = true;
            // Validate together to reject collisions before persisting.
            let loaded = Extensions::load(&config, workspace).await?;
            config
                .entries
                .last_mut()
                .context("installed extension")?
                .name = loaded.names().last().cloned();
            println!("Validated {}", loaded.names().join(", "));
        }
        ExtensionCommand::Enable { target } | ExtensionCommand::Disable { target } => {
            if let Some(target) = target {
                let index = select(&config, &target)?;
                config.entries[index].enabled = enable;
            } else {
                config.enabled = enable;
            }
        }
        ExtensionCommand::Remove { target } => {
            let index = select(&config, &target)?;
            config.entries.remove(index);
        }
    }
    save(&path, &config)?;
    println!("Saved. Running workspace servers apply extension changes when active work is idle.");
    Ok(())
}

async fn install_npm(paths: &Paths, spec: &str) -> Result<PathBuf> {
    if spec.is_empty()
        || spec.starts_with('-')
        || !spec
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"@/._-+".contains(&byte))
    {
        bail!("use npm:package or npm:@scope/package@version (registry packages only)");
    }
    let name = if let Some((name, _)) = spec.rsplit_once('@') {
        if name.is_empty() { spec } else { name }
    } else {
        spec
    };
    if name.contains("..")
        || name.starts_with('/')
        || (name.contains('/') && (!name.starts_with('@') || name.matches('/').count() != 1))
    {
        bail!("invalid npm package name");
    }
    let root = paths
        .data
        .join("extensions/packages")
        .join(uuid::Uuid::now_v7().to_string());
    std::fs::create_dir_all(&root)?;
    std::fs::write(root.join("package.json"), "{\"private\":true}")?;
    let mut command = tokio::process::Command::new(haarness_extensions::bun()?);
    command
        .args(["add", "--ignore-scripts", "--no-progress", "--exact", spec])
        .current_dir(&root)
        .env_remove("NODE_OPTIONS")
        .env_remove("BUN_OPTIONS")
        .kill_on_drop(true);
    let status = tokio::time::timeout(std::time::Duration::from_secs(120), command.status())
        .await
        .context("npm extension install timed out")??;
    if !status.success() {
        bail!("npm extension install failed; no extension was enabled");
    }
    haarness_extensions::entrypoint(&root.join("node_modules").join(name))
}

/// Watch code and manifests, not files an extension writes while handling events.
pub fn watch_sources(source: &Path, roots: &mut Vec<PathBuf>) -> Result<()> {
    fn visit(path: &Path, roots: &mut Vec<PathBuf>, depth: usize) -> Result<()> {
        if depth > 16 {
            return Ok(());
        }
        let metadata = std::fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() {
            roots.push(path.to_path_buf());
            return Ok(());
        }
        if metadata.is_dir() {
            if matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("node_modules" | "target" | ".git" | ".data")
            ) {
                return Ok(());
            }
            for entry in std::fs::read_dir(path)? {
                visit(&entry?.path(), roots, depth + 1)?;
            }
        } else if matches!(
            path.extension().and_then(|name| name.to_str()),
            Some("ts" | "mts" | "js" | "mjs" | "cjs" | "json" | "toml")
        ) {
            roots.push(path.to_path_buf());
        }
        Ok(())
    }
    roots.push(source.to_path_buf());
    if let Some(parent) = source.parent().filter(|parent| parent.is_dir()) {
        visit(parent, roots, 0)?;
    }
    Ok(())
}

fn select(config: &Config, target: &str) -> Result<usize> {
    if let Some(index) = config
        .entries
        .iter()
        .position(|entry| entry.name.as_deref() == Some(target))
    {
        return Ok(index);
    }
    if let Ok(index) = target.parse::<usize>() {
        if index > 0 && index <= config.entries.len() {
            return Ok(index - 1);
        }
    }
    bail!("extension {target} not found; use extension list for names and indices")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn project_extensions_override_only_the_selected_project() {
        let root =
            std::env::temp_dir().join(format!("haarness-extension-scope-{}", uuid::Uuid::now_v7()));
        let paths = Paths::new(Some(&root)).unwrap();
        let project = root.join("project");
        let other = root.join("other");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let source = root.join("shared.ts");
        std::fs::write(&source, "export default {};").unwrap();
        let entry = Entry {
            name: Some("fixture".into()),
            source,
            options: serde_json::json!({}),
            enabled: true,
        };
        save(
            &config_path(&paths),
            &Config {
                enabled: true,
                entries: vec![entry.clone()],
                ..Config::default()
            },
        )
        .unwrap();
        let inherited = scoped_config(&paths, &other, true).unwrap();
        assert!(inherited.enabled);
        assert!(inherited.entries.is_empty());
        save(
            &scoped_config_path(&paths, &project, true),
            &Config {
                enabled: true,
                entries: vec![Entry {
                    enabled: false,
                    ..entry
                }],
                ..Config::default()
            },
        )
        .unwrap();
        assert!(!sources(&paths, &project, true).unwrap().entries[0].enabled);
        assert!(sources(&paths, &other, true).unwrap().entries[0].enabled);
        assert!(sources(&paths, &project, false).unwrap().entries[0].enabled);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn configuration_declared_extensions_join_the_registration_path() {
        let root = std::env::temp_dir().join(format!(
            "haarness-extension-declared-{}",
            uuid::Uuid::now_v7()
        ));
        let paths = Paths::new(Some(&root)).unwrap();
        let workspace = root.join("project");
        std::fs::create_dir_all(workspace.join(".haarness/ext")).unwrap();
        let source = workspace.join(".haarness/ext/index.ts");
        std::fs::write(&source, "export default {apiVersion:1,name:'declared'};").unwrap();
        std::fs::write(
            workspace.join(".haarness/haarness.json"),
            r#"{"extensions":[["./ext", {"greeting":"declared"}], "not-installed-package"]}"#,
        )
        .unwrap();
        let source = source.canonicalize().unwrap();
        let config = sources(&paths, &workspace, true).unwrap();
        assert!(config.enabled, "declarations enable the extension system");
        let entry = config.entries.iter().find(|entry| entry.source == source).unwrap();
        assert!(entry.enabled);
        assert_eq!(entry.options, serde_json::json!({"greeting":"declared"}));
        assert_eq!(config.entries.len(), 1, "pending npm packages are not entries");
        assert!(sources(&paths, &workspace, false).unwrap().entries.is_empty());
        // An explicit extensions.json entry for the same source wins, and an
        // explicit global switch is respected.
        save(
            &config_path(&paths),
            &Config {
                enabled: false,
                entries: vec![Entry {
                    name: None,
                    source: source.clone(),
                    options: serde_json::Value::Null,
                    enabled: false,
                }],
                ..Config::default()
            },
        )
        .unwrap();
        let config = sources(&paths, &workspace, true).unwrap();
        assert!(!config.enabled);
        assert_eq!(config.entries.len(), 1);
        assert!(!config.entries[0].enabled);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn disabled_source_alias_suppresses_discovery() {
        let root = std::env::temp_dir().join(format!(
            "haarness-extension-discovery-{}",
            uuid::Uuid::now_v7()
        ));
        let paths = Paths::new(Some(&root)).unwrap();
        let directory = root.join("config/extensions");
        std::fs::create_dir_all(&directory).unwrap();
        let source = directory.join("fixture.ts");
        std::fs::write(&source, "throw new Error('must not execute')").unwrap();
        let alias = root.join("alias.ts");
        std::os::unix::fs::symlink(&source, &alias).unwrap();
        save(
            &config_path(&paths),
            &Config {
                enabled: true,
                entries: vec![Entry {
                    name: None,
                    source: alias,
                    enabled: false,
                    options: serde_json::Value::Null,
                }],
                ..Config::default()
            },
        )
        .unwrap();
        let config = sources(&paths, &root, false).unwrap();
        let source = source.canonicalize().unwrap();
        let entries: Vec<_> = config
            .entries
            .iter()
            .filter(|entry| entry.source == source)
            .collect();
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].enabled);
        std::fs::remove_dir_all(root).unwrap();
    }
}
