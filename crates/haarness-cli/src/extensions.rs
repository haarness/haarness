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
    #[arg(long, global = true, visible_alias = "local")]
    project: bool,
    #[command(subcommand)]
    command: ExtensionCommand,
}
#[derive(Subcommand)]
enum ExtensionCommand {
    /// Create a typed, dependency-free starter extension.
    Init {
        /// New folder for the extension.
        path: PathBuf,
        /// Extension name: lowercase letters, digits and underscores (default:
        /// the folder name).
        #[arg(long)]
        name: Option<String>,
    },
    /// Trust and install a local file/package or npm:package[@version]. Executes its initializer.
    Install {
        /// Extension file or folder, or npm:package[@version].
        source: String,
        /// Options passed to the extension, as a JSON object.
        #[arg(long, default_value = "{}")]
        options: String,
    },
    /// Show configured sources and the global enable switch without running code.
    List,
    /// Enable all extensions, or one installed extension by name or list index.
    Enable {
        /// Extension name or list number (default: all extensions).
        target: Option<String>,
    },
    /// Disable all extensions, or one installed extension by name or list index.
    Disable {
        /// Extension name or list number (default: all extensions).
        target: Option<String>,
    },
    /// Unregister an extension by name or list index; preserves source files.
    Remove {
        /// Extension name or list number.
        target: String,
    },
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
            if fields.get("timeout_ms").is_some() {
                config.timeout_ms = layer.timeout_ms;
            }
            if fields.get("init_timeout_ms").is_some() {
                config.init_timeout_ms = layer.init_timeout_ms;
            }
            if fields.get("hook_timeout_ms").is_some() {
                config.hook_timeout_ms = layer.hook_timeout_ms;
            }
            if fields.get("tool_timeout_ms").is_some() {
                config.tool_timeout_ms = layer.tool_timeout_ms;
            }
            if fields.get("payload_max_bytes").is_some() {
                config.payload_max_bytes = layer.payload_max_bytes;
            }
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
            && !config
                .entries
                .iter()
                .any(|prior| prior.source == entry.source)
        {
            config.enabled |= !explicit_switch;
            config.entries.push(entry);
        }
    }
    if !config.enabled {
        return Ok(config);
    }
    roots.extend(
        super::plugins::all_enabled_roots(&paths.data, paths.config_dir(), workspace, trust)
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

/// A valid extension name from the starter's folder name: `second-ext`
/// becomes `second_ext`. Extension names must be unique, so a fixed starter
/// name would clash with the second extension a person creates.
fn starter_name(path: &Path) -> String {
    let folder = path
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut name = String::new();
    for character in folder.chars() {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            name.push(character);
        } else if !name.is_empty() && !name.ends_with('_') {
            name.push('_');
        }
    }
    name.truncate(24);
    let name = name.trim_end_matches('_');
    if name.is_empty() {
        "hello".to_owned()
    } else {
        name.to_owned()
    }
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
    let summary = match args.command {
        ExtensionCommand::Init { path, name } => {
            let name = match name {
                Some(name) => {
                    haarness_extensions::validate_name(&name)?;
                    name
                }
                None => starter_name(&path),
            };
            std::fs::create_dir(&path).context("choose a new directory for the extension")?;
            std::fs::write(path.join("api.d.ts"), haarness_extensions::TYPES)?;
            std::fs::write(
                path.join("index.ts"),
                STARTER.replace("name: \"hello\"", &format!("name: \"{name}\"")),
            )?;
            std::fs::write(
                path.join("package.json"),
                format!(
                    "{{\"name\":\"haarness-extension-{}\",\"version\":\"0.1.0\",\"type\":\"module\",\"haarness\":{{\"extension\":\"index.ts\"}}}}\n",
                    name.replace('_', "-")
                ),
            )?;
            println!(
                "Created extension {name} in {}. Edit index.ts, then run: haarness extension install {}",
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
            if config.entries.is_empty() {
                println!(
                    "No extensions installed. Create one with `haarness extension init <folder>`."
                );
            }
            println!(
                "Use `haarness extension check` to inspect loaded tools, including discovered plugin/project extensions."
            );
            return Ok(());
        }
        ExtensionCommand::Check { verbose } => {
            let mut config = sources(paths, workspace, trust)?;
            config.log_stderr = verbose;
            let extensions = Extensions::load(&config, workspace).await?;
            let tools = extensions
                .wrap_tools(Arc::new(haarness_core::EmptyToolRegistry))
                .await?;
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
            let installed = config.entries.last_mut().context("installed extension")?;
            installed.name = loaded.names().last().cloned();
            format!("Installed {}.", entry_label(installed))
        }
        ExtensionCommand::Enable { target } | ExtensionCommand::Disable { target } => {
            let verb = if enable { "Enabled" } else { "Disabled" };
            if let Some(target) = target {
                let index = select(&config, &target)?;
                config.entries[index].enabled = enable;
                format!("{verb} {}.", entry_label(&config.entries[index]))
            } else {
                config.enabled = enable;
                format!("{verb} extensions.")
            }
        }
        ExtensionCommand::Remove { target } => {
            let index = select(&config, &target)?;
            let removed = config.entries.remove(index);
            format!("Removed {}; its files were kept.", entry_label(&removed))
        }
    };
    save(&path, &config)?;
    println!("{summary} Running Haarness servers pick this up once no turn is running.");
    Ok(())
}

async fn install_npm(paths: &Paths, spec: &str) -> Result<PathBuf> {
    install_npm_source(paths, spec, npm_package_name(spec)?).await
}

fn npm_package_name(spec: &str) -> Result<&str> {
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
    Ok(name)
}

// A mirrored marketplace package is passed as a verified local tarball. Bun may
// resolve its dependencies, but never re-fetches the top-level package by name.
async fn install_npm_source(paths: &Paths, source: &str, name: &str) -> Result<PathBuf> {
    let root = paths
        .data
        .join("extensions/packages")
        .join(uuid::Uuid::now_v7().to_string());
    std::fs::create_dir_all(&root)?;
    std::fs::write(root.join("package.json"), "{\"private\":true}")?;
    let mut command = tokio::process::Command::new(haarness_extensions::bun()?);
    command
        .args([
            "add",
            "--ignore-scripts",
            "--no-progress",
            "--exact",
            source,
        ])
        .current_dir(&root)
        .env_remove("NODE_OPTIONS")
        .env_remove("BUN_OPTIONS")
        .kill_on_drop(true);
    let status = tokio::time::timeout(std::time::Duration::from_secs(120), command.output())
        .await
        .context("npm extension install timed out")??;
    if !status.status.success() {
        bail!("npm extension install failed; no extension was enabled");
    }
    haarness_extensions::entrypoint(&root.join("node_modules").join(name))
}

fn normalize_entries(config: &mut Config, config_path: &Path) {
    for entry in &mut config.entries {
        if entry.source.is_relative() {
            entry.source = config_path
                .parent()
                .unwrap_or(Path::new("."))
                .join(&entry.source);
        }
        if let Ok(path) = entry.source.canonicalize() {
            entry.source = path;
        }
    }
}

pub fn catalog(paths: &Paths, workspace: &Path, trust: bool) -> Result<serde_json::Value> {
    let mut scoped = Vec::new();
    for local in [false, true] {
        if local && !trust {
            continue;
        }
        let path = scoped_config_path(paths, workspace, local);
        let mut config = Config::read(&path)?;
        normalize_entries(&mut config, &path);
        scoped.push((local, config));
    }
    let entries=sources(paths,workspace,trust)?.entries.into_iter().map(|entry| {
        let scope=scoped.iter().rev().find(|(_,config)|config.entries.iter().any(|e|e.source==entry.source)).map(|(local,_)|if *local {"local"} else {"global"});
        let name=entry.name.as_deref().unwrap_or_else(||entry.source.file_stem().and_then(|s|s.to_str()).unwrap_or("extension"));
        serde_json::json!({"name":name,"source":entry.source,"enabled":entry.enabled,"scope":scope,"managed":scope.is_some(),"readOnly":scope.is_none()})
    }).collect::<Vec<_>>();
    Ok(serde_json::json!(entries))
}

/// The npm root a marketplace install created for itself,
/// `<data>/extensions/packages/<uuid>`, named by the receipt's package or entry path.
pub(crate) fn marketplace_package_root(
    paths: &Paths,
    receipt: &serde_json::Value,
) -> Option<PathBuf> {
    let packages = paths.data.join("extensions/packages");
    let installed = receipt["packageRoot"]
        .as_str()
        .or(receipt["path"].as_str())?;
    let relative = Path::new(installed).strip_prefix(&packages).ok()?;
    if relative
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return None;
    }
    let first = relative.components().next()?.as_os_str().to_str()?;
    uuid::Uuid::parse_str(first).ok()?;
    Some(packages.join(first))
}

/// Uninstalling or replacing a marketplace extension deletes the package it
/// installed; only the configuration entry used to go, leaving the code behind.
pub(crate) fn remove_marketplace_package(paths: &Paths, receipt: &serde_json::Value) -> Result<()> {
    if let Some(root) = marketplace_package_root(paths, receipt)
        && root.is_dir()
    {
        haarness_profiles::check_path(&root)?;
        std::fs::remove_dir_all(root)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn marketplace_install(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    local: bool,
    manifest: &serde_json::Value,
    input: &serde_json::Value,
    old: Option<&serde_json::Value>,
    receipt_file: &Path,
    archive: Option<&Path>,
) -> Result<serde_json::Value> {
    use anyhow::ensure;
    use serde_json::json;
    let package = &manifest["install"]["extension"];
    ensure!(
        package["extension_type"] == "core",
        "Only Haarness core extensions can be loaded into the runtime"
    );
    let spec = package["npm_spec"]
        .as_str()
        .context("Missing npm package spec")?;
    let (package_name, version) = spec
        .rsplit_once('@')
        .context("Marketplace extensions require an exact package version")?;
    npm_package_name(spec)?;
    semver::Version::parse(version)
        .context("Marketplace extensions require an exact package version")?;
    let integrity = package["integrity"]
        .as_str()
        .filter(|s| s.starts_with("sha512-"))
        .context("Marketplace extensions require SHA-512 integrity")?;
    let source = if let Some(archive) = archive {
        install_npm_source(
            paths,
            archive.to_str().context("Invalid package archive path")?,
            package_name,
        )
        .await?
    } else {
        // Compatibility with catalogs published before R2-backed npm artifacts.
        install_npm(paths, spec).await?
    };
    // A package that fails any check below never stays installed.
    let result = async {
        // Verify the catalog's digest before executing the extension initializer.
        let install_root = source
            .ancestors()
            .find(|p| p.join("bun.lock").is_file())
            .context("Missing npm installation lock")?;
        let package_root = install_root.join("node_modules").join(package_name);
        let metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(package_root.join("package.json"))?)?;
        ensure!(
            metadata["name"] == package_name && metadata["version"] == version,
            "Installed package identity does not match marketplace version"
        );
        if archive.is_none() {
            let lock_text = std::fs::read_to_string(install_root.join("bun.lock"))?;
            let lock = jsonc_parser::parse_to_serde_value::<serde_json::Value>(
                &lock_text,
                &jsonc_parser::ParseOptions::default(),
            )?;
            let pinned = &lock["packages"][package_name];
            ensure!(
                pinned.as_array().is_some_and(|items| items
                    .first()
                    .and_then(serde_json::Value::as_str)
                    == Some(spec)
                    && items.iter().any(|v| v == integrity)),
                "Installed npm package does not match marketplace integrity"
            );
        }
        let path = scoped_config_path(paths, workspace, local);
        // The marketplace lock first, as removal takes them: the receipt as
        // it is now decides, not as it was before the download.
        let _receipts = crate::desktop::editor_lock(&paths.data, "marketplace")?;
        let current =
            crate::marketplace::receipt_now(paths, workspace, receipt_file, old, input)?;
        let old = current.as_ref();
        let _lock = crate::desktop::editor_lock(&paths.data, "extensions")?;
        let previous = std::fs::read(&path).ok();
        let mut config = scoped_config(paths, workspace, local)?;
        normalize_entries(&mut config, &path);
        let mut enabled = true;
        let mut options = input.get("options").cloned();
        if let Some(old) = old {
            let old_path = Path::new(old["path"].as_str().context("Invalid extension receipt")?);
            let digest_path = Path::new(
                old["packageRoot"]
                    .as_str()
                    .unwrap_or(old["path"].as_str().unwrap_or_default()),
            );
            ensure!(
                digest_path.starts_with(paths.data.join("extensions/packages"))
                    && !digest_path
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir)),
                "Invalid extension package root"
            );
            ensure!(
                crate::marketplace::tree_digest(digest_path)? == old["installedDigest"],
                "Extension was modified locally"
            );
            if let Some(entry) = config.entries.iter().find(|e| e.source == old_path) {
                enabled = entry.enabled;
                if options.is_none() {
                    options = Some(entry.options.clone());
                }
            }
            config.entries.retain(|e| e.source != old_path);
        }
        let entry = Entry {
            name: None,
            source: source.clone(),
            options: options.unwrap_or(json!({})),
            enabled: true,
        };
        // Include inherited extensions when checking duplicate tools and names.
        let mut validation = sources(paths, workspace, trust)?;
        if let Some(old) = old {
            validation
                .entries
                .retain(|e| Some(e.source.as_path()) != old["path"].as_str().map(Path::new));
        }
        validation.entries.push(entry.clone());
        validation.enabled = true;
        let loaded = Extensions::load(&validation, workspace).await?;
        let name = loaded
            .names()
            .last()
            .cloned()
            .context("Extension did not register a name")?;
        config.entries.push(Entry {
            name: Some(name.clone()),
            enabled,
            ..entry
        });
        // A first install turns extensions on; an update leaves the switch
        // as the person set it.
        if old.is_none() {
            config.enabled = true;
        }
        let receipt = json!({"id":manifest["resource"]["id"],"kind":"extension","name":name,"publisher":manifest["resource"]["publisher"],"slug":manifest["resource"]["slug"],"scope":if local {"local"} else {"global"},"path":source,"packageRoot":package_root,"version":manifest["version"],"lock":manifest["lock"],"installedDigest":crate::marketplace::tree_digest(&package_root)?,"autoUpdate":crate::marketplace::auto_update_choice(input, old),"restartRequired":false});
        save(&path, &config)?;
        if let Err(error) =
            haarness_profiles::write(receipt_file, &serde_json::to_vec_pretty(&receipt)?, true)
        {
            if let Some(bytes) = previous {
                haarness_profiles::write(&path, &bytes, true)?;
            } else {
                std::fs::remove_file(&path)?;
            }
            return Err(error);
        }
        if let Some(old) = old
            && marketplace_package_root(paths, old) != marketplace_package_root(paths, &receipt)
        {
            remove_marketplace_package(paths, old)?;
        }
        Ok(receipt)
    }
    .await;
    if result.is_err()
        && let Some(root) = marketplace_package_root(paths, &json!({ "path": source }))
    {
        let _ = std::fs::remove_dir_all(root);
    }
    result
}

pub fn manage(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    input: &serde_json::Value,
) -> Result<serde_json::Value> {
    use anyhow::ensure;
    let action = input["action"]
        .as_str()
        .context("Extension action required")?;
    let local = input["scope"] != "global";
    let path = scoped_config_path(paths, workspace, local);
    let _lock = crate::desktop::editor_lock(&paths.data, "extensions")?;
    let mut config = scoped_config(paths, workspace, local)?;
    normalize_entries(&mut config, &path);
    if action == "list" {
        return Ok(serde_json::to_value(config)?);
    }
    let name = input["name"].as_str().context("Extension name required")?;
    ensure!(
        matches!(action, "enable" | "disable" | "remove"),
        "Use the marketplace or extension install command to install an extension"
    );
    let matches = |e: &Entry| {
        e.name.as_deref() == Some(name)
            || e.source.to_str() == Some(name)
            || e.name.is_none() && e.source.file_stem().and_then(|s| s.to_str()) == Some(name)
    };
    let index = if let Some(index) = config.entries.iter().position(matches) {
        index
    } else {
        ensure!(
            action != "remove",
            "Inherited extensions can be disabled; remove them from their owning plugin or scope"
        );
        let entries = sources(paths, workspace, trust)?
            .entries
            .into_iter()
            .filter(matches)
            .collect::<Vec<_>>();
        ensure!(
            entries.len() == 1,
            "Select a unique installed extension by name or source path"
        );
        config.entries.push(entries[0].clone());
        config.entries.len() - 1
    };
    if action == "remove" {
        config.entries.remove(index);
    } else {
        config.entries[index].enabled = action == "enable";
        if action == "enable" {
            config.enabled = true;
        }
    }
    save(&path, &config)?;
    Ok(serde_json::json!({"action":action,"name":name,"restartRequired":false}))
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

fn entry_label(entry: &Entry) -> String {
    entry
        .name
        .clone()
        .unwrap_or_else(|| entry.source.display().to_string())
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
    bail!("extension {target} not found; use `haarness extension list` for names and indices")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn starter_extensions_are_named_after_their_folder() {
        assert_eq!(starter_name(Path::new("/tmp/second-ext")), "second_ext");
        assert_eq!(starter_name(Path::new("My Tools!")), "my_tools");
        assert_eq!(starter_name(Path::new("--")), "hello");
        let long = starter_name(Path::new("a-very-long-extension-folder-name"));
        assert!(haarness_extensions::validate_name(&long).is_ok(), "{long}");
        assert!(haarness_extensions::validate_name(&starter_name(Path::new("x__y"))).is_ok());
    }

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
        let entry = config
            .entries
            .iter()
            .find(|entry| entry.source == source)
            .unwrap();
        assert!(entry.enabled);
        assert_eq!(entry.options, serde_json::json!({"greeting":"declared"}));
        assert_eq!(
            config.entries.len(),
            1,
            "pending npm packages are not entries"
        );
        assert!(
            sources(&paths, &workspace, false)
                .unwrap()
                .entries
                .is_empty()
        );
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
