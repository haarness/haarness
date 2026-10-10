//! Plugin bundles and Claude-Code-format marketplaces.
//!
//! A plugin is a directory with `.claude-plugin/plugin.json` plus any of
//! `commands/`, `agents/`, `skills/`, `hooks/hooks.json`, `.mcp.json` — the
//! same layout Claude Code plugins use. A marketplace is a git repo (or
//! local directory) with `.claude-plugin/marketplace.json`:
//! `{"name": …, "plugins": [{"name", "source", "description"}]}` where
//! `source` is a path inside the repo or a git URL.
//!
//! State lives under `<data>/plugins/`:
//! - `marketplaces.json` — name → local path (clone or original dir)
//! - `installed.json`    — name → { marketplace, path, enabled }
//! - `marketplaces/<name>/`, `cache/<name>/` — cloned content

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use haarness_protocol::{PluginActionRequest, PluginInfo};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPlugin {
    pub marketplace: String,
    pub path: PathBuf,
    pub enabled: bool,
    #[serde(default)]
    pub description: String,
}

fn plugins_root(data: &Path) -> PathBuf {
    data.join("plugins")
}

fn read_map<T: serde::de::DeserializeOwned>(path: &Path) -> BTreeMap<String, T> {
    fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

fn write_map<T: Serialize>(path: &Path, map: &BTreeMap<String, T>) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(map)?)?;
    Ok(())
}

fn marketplaces_file(data: &Path) -> PathBuf {
    plugins_root(data).join("marketplaces.json")
}
fn installed_file(data: &Path) -> PathBuf {
    plugins_root(data).join("installed.json")
}

pub fn installed(data: &Path) -> BTreeMap<String, InstalledPlugin> {
    read_map(&installed_file(data))
}

/// Directories of every enabled plugin, before configuration overrides.
#[cfg(test)]
pub fn enabled_roots(data: &Path) -> Vec<PathBuf> {
    installed(data)
        .values()
        .filter(|plugin| plugin.enabled && plugin.path.exists())
        .map(|plugin| plugin.path.clone())
        .collect()
}

/// Only Haarness-managed plugin roots participate in runtime discovery.
pub fn all_enabled_roots(
    data: &Path,
    config: &Path,
    workspace: &Path,
    trust_project: bool,
) -> Vec<PathBuf> {
    // `plugins: [{spec, enabled: false}]` in configuration hides an installed
    // plugin without rewriting the install record.
    let disabled = crate::config_specs::disabled_plugins(config, workspace, trust_project);
    let mut roots: Vec<PathBuf> = installed(data)
        .into_iter()
        .filter(|(name, plugin)| plugin.enabled && plugin.path.exists() && !disabled.contains(name))
        .map(|(_, plugin)| plugin.path)
        .collect();
    if !config.join(".layout-v1").exists() && !data.join(".layout-v1").exists() {
        roots.extend(local_plugin_roots(&data.join("plugins/local")));
    }
    roots.extend(local_plugin_roots(&config.join("plugins/local")));
    if trust_project {
        for directory in crate::layout::project_directories(workspace) {
            roots.extend(local_plugin_roots(&directory.join(".haarness/plugins")));
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

fn local_plugin_roots(root: &Path) -> Vec<PathBuf> {
    let mut roots = local_plugin_directories(root, 0)
        .into_iter()
        .filter(|path| !path.join(".haarness-disabled").exists())
        .collect::<Vec<_>>();
    roots.sort();
    roots
}

fn local_plugin_infos(root: &Path, scope: &str) -> Vec<PluginInfo> {
    let mut infos = Vec::new();
    for path in local_plugin_directories(root, 0) {
        let manifest_path = path.join(".claude-plugin/plugin.json");
        let Some(manifest) = fs::read_to_string(&manifest_path)
            .ok()
            .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
        else {
            continue;
        };
        let Some(name) = manifest
            .get("name")
            .and_then(serde_json::Value::as_str)
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        infos.push(PluginInfo {
            name: name.into(),
            spec: name.into(),
            source: format!("haarness-{scope}"),
            marketplace: None,
            path: path.display().to_string(),
            enabled: !path.join(".haarness-disabled").exists(),
            managed: true,
            description: manifest
                .get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .into(),
        });
    }
    infos.sort_by(|a, b| a.name.cmp(&b.name));
    infos
}

fn local_plugin_directories(root: &Path, depth: usize) -> Vec<PathBuf> {
    if depth > 16 {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for path in entries.flatten().map(|entry| entry.path()) {
        if path.join(".claude-plugin/plugin.json").is_file() {
            values.push(path);
        } else if path.is_dir() {
            values.extend(local_plugin_directories(&path, depth + 1));
        }
    }
    values.sort();
    values
}

fn resolve_local_plugin(
    data: &Path,
    config: &Path,
    workspace: &Path,
    name: &str,
) -> Option<PathBuf> {
    [
        workspace.join(".haarness/plugins"),
        config.join("plugins/local"),
        data.join("plugins/local"),
    ]
    .into_iter()
    .flat_map(|root| local_plugin_directories(&root, 0))
    .find(|path| {
        fs::read_to_string(path.join(".claude-plugin/plugin.json"))
            .ok()
            .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
            .and_then(|manifest| {
                manifest
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .as_deref()
            == Some(name)
    })
}

pub fn set_any_enabled(
    data: &Path,
    config: &Path,
    workspace: &Path,
    name: &str,
    enabled: bool,
) -> Result<()> {
    if let Some(path) = resolve_local_plugin(data, config, workspace, name) {
        let marker = path.join(".haarness-disabled");
        if enabled {
            match fs::remove_file(marker) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        } else {
            fs::write(marker, "disabled by haarness\n")?;
        }
        return Ok(());
    }
    set_enabled(data, name, enabled)
}

pub fn remove_any(data: &Path, config: &Path, workspace: &Path, name: &str) -> Result<()> {
    if let Some(path) = resolve_local_plugin(data, config, workspace, name) {
        fs::remove_dir_all(path)?;
        return Ok(());
    }
    remove(data, name)
}

pub fn plugin_infos(
    data: &Path,
    config: &Path,
    workspace: &Path,
    project_config: bool,
) -> Vec<PluginInfo> {
    let disabled = crate::config_specs::disabled_plugins(config, workspace, project_config);
    let mut result: Vec<_> = installed(data)
        .into_iter()
        .map(|(name, plugin)| PluginInfo {
            spec: format!("{name}@{}", plugin.marketplace),
            enabled: plugin.enabled && !disabled.contains(&name),
            name,
            source: "haarness".into(),
            marketplace: Some(plugin.marketplace),
            path: plugin.path.display().to_string(),
            managed: true,
            description: plugin.description,
        })
        .collect();
    result.extend(local_plugin_infos(&config.join("plugins/local"), "global"));
    if !config.join(".layout-v1").exists() && !data.join(".layout-v1").exists() {
        for legacy in local_plugin_infos(&data.join("plugins/local"), "global") {
            if !result.iter().any(|item| item.name == legacy.name) {
                result.push(legacy);
            }
        }
    }
    if project_config {
        result.extend(local_plugin_infos(
            &workspace.join(".haarness/plugins"),
            "local",
        ));
    }
    // Plugin inventory includes only Haarness-owned installations and roots.
    result.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.source.cmp(&b.source)));
    result
}

pub struct PluginService {
    data: PathBuf,
    config: PathBuf,
    workspace: PathBuf,
    project_config: bool,
}

impl PluginService {
    pub fn new(data: PathBuf, config: PathBuf, workspace: PathBuf, project_config: bool) -> Self {
        Self {
            data,
            config,
            workspace,
            project_config,
        }
    }
}

impl haarness_server::PluginRegistry for PluginService {
    fn list(&self) -> Result<Vec<PluginInfo>> {
        Ok(plugin_infos(
            &self.data,
            &self.config,
            &self.workspace,
            self.project_config,
        ))
    }

    fn apply(&self, request: &PluginActionRequest) -> Result<Vec<PluginInfo>> {
        match request.action.as_str() {
            "marketplace" => {
                marketplace_add(&self.data, &request.target)?;
            }
            "install" => {
                install(&self.data, &request.target)?;
            }
            "update" => {
                if resolve_local_plugin(&self.data, &self.config, &self.workspace, &request.target)
                    .is_some()
                    && !installed(&self.data).contains_key(&request.target)
                {
                    bail!(
                        "local plugin {} uses its source directory directly; refresh Haarness after editing it",
                        request.target
                    );
                }
                update(&self.data, &request.target)?;
            }
            "enable" => set_any_enabled(
                &self.data,
                &self.config,
                &self.workspace,
                &request.target,
                true,
            )?,
            "disable" => set_any_enabled(
                &self.data,
                &self.config,
                &self.workspace,
                &request.target,
                false,
            )?,
            "remove" | "uninstall" => {
                remove_any(&self.data, &self.config, &self.workspace, &request.target)?
            }
            action => bail!(
                "unknown plugin action {action}; use install, update, enable, disable, remove, or marketplace"
            ),
        }
        self.list()
    }
}

#[derive(Deserialize)]
struct MarketplaceManifest {
    name: String,
    #[serde(default)]
    plugins: Vec<MarketplacePlugin>,
}

#[derive(Deserialize)]
struct MarketplacePlugin {
    name: String,
    source: serde_json::Value,
    #[serde(default)]
    description: String,
}

fn manifest_of(root: &Path) -> Result<MarketplaceManifest> {
    let path = root.join(".claude-plugin/marketplace.json");
    let content = fs::read_to_string(&path)
        .with_context(|| format!("no marketplace manifest at {}", path.display()))?;
    Ok(serde_json::from_str(&content)?)
}

fn safe_name(name: &str, kind: &str) -> Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.chars().any(char::is_control)
    {
        bail!("invalid {kind} name {name:?}");
    }
    Ok(())
}

/// A path below a source root. Root itself is intentionally excluded so this
/// remains safe for git subdirectories and any future metadata root field.
fn safe_relative(value: &str) -> bool {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return false;
    }
    let normalized = value.replace('\\', "/");
    if normalized.starts_with('/')
        || normalized.starts_with("//")
        || normalized.as_bytes().get(1) == Some(&b':')
    {
        return false;
    }
    let parts = normalized.split('/').collect::<Vec<_>>();
    !parts.contains(&"..") && parts.iter().any(|part| !part.is_empty() && *part != ".")
}

/// A marketplace is allowed to be the plugin root. Keep this exception local
/// to string plugin sources; `git-subdir` must still identify a real child.
fn safe_relative_plugin_source(value: &str) -> bool {
    matches!(value, "." | "./") || safe_relative(value)
}

fn git_url(source: &str) -> String {
    if source.contains("://")
        || source.starts_with("git@")
        || Path::new(source)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("git"))
    {
        return source.to_owned();
    }
    if source.matches('/').count() == 1 {
        return format!("https://github.com/{source}.git");
    }
    source.to_owned()
}

fn git_clone(url: &str, destination: &Path) -> Result<()> {
    let parent = destination.parent().unwrap_or(destination);
    fs::create_dir_all(parent)?;
    let leaf = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("plugin");
    let nonce = uuid::Uuid::now_v7();
    let staging = parent.join(format!(".{leaf}.stage-{nonce}"));
    let backup = parent.join(format!(".{leaf}.backup-{nonce}"));
    let status = Command::new("git")
        .args(["clone", "--depth", "1", url])
        .arg(&staging)
        .status()
        .context("running git clone")?;
    if !status.success() {
        let _ = fs::remove_dir_all(&staging);
        bail!("git clone failed for {url}");
    }
    if destination.exists() {
        fs::rename(destination, &backup)?;
    }
    if let Err(error) = fs::rename(&staging, destination) {
        if backup.exists() {
            let _ = fs::rename(&backup, destination);
        }
        let _ = fs::remove_dir_all(&staging);
        return Err(error.into());
    }
    if backup.exists() {
        fs::remove_dir_all(backup)?;
    }
    Ok(())
}

pub(crate) fn validate_plugin_tree(root: &Path) -> Result<()> {
    let root = root.canonicalize()?;
    let mut pending = vec![root.clone()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            if entry.file_name() == ".git" {
                continue;
            }
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                let resolved = path.canonicalize().with_context(|| {
                    format!(
                        "plugin symlink target cannot be resolved: {}",
                        path.display()
                    )
                })?;
                if !resolved.starts_with(&root) {
                    bail!("plugin symlink escapes its root: {}", path.display());
                }
            } else if metadata.is_dir() {
                pending.push(path);
            } else if !metadata.is_file() {
                bail!(
                    "plugin contains unsupported filesystem entry: {}",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn copy_symlink(source: &Path, destination: &Path) -> Result<()> {
    std::os::unix::fs::symlink(fs::read_link(source)?, destination)?;
    Ok(())
}

#[cfg(windows)]
fn copy_symlink(source: &Path, destination: &Path) -> Result<()> {
    let target = fs::read_link(source)?;
    if fs::metadata(source)?.is_dir() {
        std::os::windows::fs::symlink_dir(target, destination)?;
    } else {
        std::os::windows::fs::symlink_file(target, destination)?;
    }
    Ok(())
}

fn copy_plugin_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    fs::set_permissions(destination, fs::metadata(source)?.permissions())?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if entry.file_name() == ".git" {
            continue;
        }
        let source = entry.path();
        let destination = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source)?;
        if metadata.file_type().is_symlink() {
            copy_symlink(&source, &destination)?;
        } else if metadata.is_dir() {
            copy_plugin_tree(&source, &destination)?;
        } else if metadata.is_file() {
            fs::copy(&source, &destination)?;
        } else {
            bail!(
                "plugin contains unsupported filesystem entry: {}",
                source.display()
            );
        }
    }
    Ok(())
}

fn materialize_plugin_tree(source: &Path, destination: &Path) -> Result<()> {
    validate_plugin_tree(source)?;
    let parent = destination.parent().unwrap_or(destination);
    fs::create_dir_all(parent)?;
    let leaf = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("plugin");
    let nonce = uuid::Uuid::now_v7();
    let staging = parent.join(format!(".{leaf}.stage-{nonce}"));
    let backup = parent.join(format!(".{leaf}.backup-{nonce}"));
    if let Err(error) =
        copy_plugin_tree(source, &staging).and_then(|()| validate_plugin_tree(&staging))
    {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    if destination.exists() {
        fs::rename(destination, &backup)?;
    }
    if let Err(error) = fs::rename(&staging, destination) {
        if backup.exists() {
            let _ = fs::rename(&backup, destination);
        }
        let _ = fs::remove_dir_all(&staging);
        return Err(error.into());
    }
    if backup.exists() {
        fs::remove_dir_all(backup)?;
    }
    Ok(())
}

/// Register a marketplace from a git URL or a local directory.
pub fn marketplace_add(data: &Path, source: &str) -> Result<String> {
    let local = local_path(source).unwrap_or_else(|| PathBuf::from(source));
    let root = if local.exists() {
        local.canonicalize()?
    } else if local_path(source).is_some() {
        bail!("{source} does not exist");
    } else {
        // Normalize owner/repo shorthand to a GitHub URL.
        let url = git_url(source);
        let staging = plugins_root(data).join("marketplaces/.staging");
        git_clone(&url, &staging)?;
        let name = manifest_of(&staging)?.name;
        safe_name(&name, "marketplace")?;
        let destination = plugins_root(data).join("marketplaces").join(&name);
        if destination.exists() {
            fs::remove_dir_all(&destination)?;
        }
        fs::rename(&staging, &destination)?;
        destination
    };
    let manifest = manifest_of(&root)?;
    safe_name(&manifest.name, "marketplace")?;
    let mut marketplaces: BTreeMap<String, PathBuf> = read_map(&marketplaces_file(data));
    marketplaces.insert(manifest.name.clone(), root);
    write_map(&marketplaces_file(data), &marketplaces)?;
    Ok(manifest.name)
}

/// A source that names a folder on this computer: absolute, `~/…`, `./…` or
/// `../…`. Anything else may be a plugin name or an `owner/repo` shorthand.
fn local_path(spec: &str) -> Option<PathBuf> {
    if let Some(rest) = spec.strip_prefix("~/") {
        return std::env::var_os("HOME").map(|home| PathBuf::from(home).join(rest));
    }
    let relative = [".", ".."].contains(&spec) || spec.starts_with("./") || spec.starts_with("../");
    (relative || Path::new(spec).is_absolute()).then(|| PathBuf::from(spec))
}

/// Install `name@marketplace` (or `name` when only one marketplace exists),
/// or register a local Claude-format plugin directory/plugin.json directly.
pub fn install(data: &Path, spec: &str) -> Result<InstalledPlugin> {
    let local = local_path(spec).unwrap_or_else(|| PathBuf::from(spec));
    let local = local.as_path();
    if local.exists() {
        let path = if local.is_file()
            && local.file_name().and_then(|name| name.to_str()) == Some("plugin.json")
        {
            local
                .parent()
                .and_then(Path::parent)
                .context("plugin.json must live under .claude-plugin")?
                .canonicalize()?
        } else {
            local.canonicalize()?
        };
        if !path.is_dir() {
            bail!("plugin source {} is not a directory", path.display());
        }
        let manifest: serde_json::Value =
            fs::read_to_string(path.join(".claude-plugin/plugin.json"))
                .context("local plugin needs .claude-plugin/plugin.json")
                .and_then(|content| serde_json::from_str(&content).map_err(Into::into))?;
        let name = manifest
            .get("name")
            .and_then(serde_json::Value::as_str)
            .filter(|name| !name.is_empty())
            .context("local plugin manifest needs name")?
            .to_owned();
        safe_name(&name, "plugin")?;
        let plugin = InstalledPlugin {
            marketplace: "local".into(),
            path,
            enabled: true,
            description: manifest
                .get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        };
        let mut plugins = installed(data);
        plugins.insert(name, plugin.clone());
        write_map(&installed_file(data), &plugins)?;
        return Ok(plugin);
    }
    // Plugin names never contain a path separator.
    let name = spec.split_once('@').map_or(spec, |(name, _)| name);
    if local_path(spec).is_some() || name.contains(['/', '\\']) {
        bail!("{spec} does not exist");
    }
    let marketplaces: BTreeMap<String, PathBuf> = read_map(&marketplaces_file(data));
    let (name, market) = match spec.split_once('@') {
        Some((name, market)) => (name.to_owned(), market.to_owned()),
        None => {
            if marketplaces.len() == 1 {
                let market = marketplaces.keys().next().cloned().unwrap_or_default();
                (spec.to_owned(), market)
            } else if marketplaces.is_empty() {
                bail!(
                    "{spec} is not a folder on this computer, and no plugin marketplace has been added to install it from"
                );
            } else {
                bail!(
                    "Say which marketplace to install {spec} from: {spec}@<marketplace> (added: {})",
                    marketplaces.keys().cloned().collect::<Vec<_>>().join(", ")
                );
            }
        }
    };
    safe_name(&name, "plugin")?;
    safe_name(&market, "marketplace")?;
    let root = marketplaces
        .get(&market)
        .with_context(|| format!("unknown marketplace {market}; add it first"))?;
    let manifest = manifest_of(root)?;
    let entry = manifest
        .plugins
        .iter()
        .find(|plugin| plugin.name == name)
        .with_context(|| format!("marketplace {market} has no plugin {name}"))?;
    let cache = plugins_root(data).join("cache").join(&market).join(&name);
    let path = match &entry.source {
        serde_json::Value::String(source) => {
            if source.contains("://")
                || source.starts_with("git@")
                || source.to_ascii_lowercase().ends_with(".git")
            {
                git_clone(source, &cache)?;
                cache.clone()
            } else {
                if !safe_relative_plugin_source(source) {
                    bail!("plugin {name} has an unsafe relative source path");
                }
                let candidate = root.join(source);
                let root = root.canonicalize()?;
                let candidate = candidate.canonicalize().with_context(|| {
                    format!("plugin source {} does not exist", candidate.display())
                })?;
                if !candidate.starts_with(&root) {
                    bail!(
                        "plugin source must stay inside marketplace {}",
                        root.display()
                    );
                }
                if !candidate.is_dir() {
                    bail!("plugin source {} does not exist", candidate.display());
                }
                materialize_plugin_tree(&candidate, &cache)?;
                cache.clone()
            }
        }
        serde_json::Value::Object(object) => {
            let kind = object
                .get("source")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("github");
            let source = match kind {
                "github" => object
                    .get("repo")
                    .and_then(serde_json::Value::as_str)
                    .map(git_url)
                    .context("github plugin source needs repo")?,
                "url" | "git-subdir" => object
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .map(git_url)
                    .context("plugin source needs url")?,
                other => bail!("unsupported plugin source type {other} for {name}"),
            };
            let subdir = if kind == "git-subdir" {
                let subdir = object
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .context("git-subdir plugin source needs path")?;
                if !safe_relative(subdir) {
                    bail!("plugin {name} has an unsafe git subdirectory path");
                }
                Some(subdir)
            } else {
                None
            };
            git_clone(&source, &cache)?;
            if let Some(subdir) = subdir {
                let target = cache.join(subdir);
                let canonical_cache = cache.canonicalize()?;
                let target = target.canonicalize().with_context(|| {
                    format!("plugin subdirectory {} does not exist", target.display())
                })?;
                if !target.starts_with(&canonical_cache) || !target.is_dir() {
                    bail!("plugin subdirectory must stay inside its repository");
                }
                target
            } else {
                cache.clone()
            }
        }
        _ => bail!("unsupported plugin source for {name}"),
    };
    if !path.join(".claude-plugin/plugin.json").exists() && !path.join("commands").exists() {
        bail!(
            "{} does not look like a plugin (no .claude-plugin/plugin.json)",
            path.display()
        );
    }
    let plugin = InstalledPlugin {
        marketplace: market,
        path,
        enabled: true,
        description: entry.description.clone(),
    };
    let mut plugins = installed(data);
    plugins.insert(name, plugin.clone());
    write_map(&installed_file(data), &plugins)?;
    Ok(plugin)
}

pub fn set_enabled(data: &Path, name: &str, enabled: bool) -> Result<()> {
    let mut plugins = installed(data);
    let plugin = plugins
        .get_mut(name)
        .with_context(|| format!("plugin {name} is not installed"))?;
    plugin.enabled = enabled;
    write_map(&installed_file(data), &plugins)
}

pub fn update(data: &Path, name: &str) -> Result<InstalledPlugin> {
    let plugin = installed(data)
        .get(name)
        .cloned()
        .with_context(|| format!("plugin {name} is not installed"))?;
    if plugin.marketplace == "local" {
        if !plugin.path.is_dir() {
            bail!(
                "local plugin source {} no longer exists",
                plugin.path.display()
            );
        }
        return Ok(plugin);
    }
    let mut updated = install(data, &format!("{name}@{}", plugin.marketplace))?;
    if !plugin.enabled {
        set_enabled(data, name, false)?;
        updated.enabled = false;
    }
    Ok(updated)
}

pub fn remove(data: &Path, name: &str) -> Result<()> {
    let mut plugins = installed(data);
    let plugin = plugins
        .remove(name)
        .with_context(|| format!("plugin {name} is not installed"))?;
    // Only delete a cache owned by this install. Relative marketplace paths
    // can be the marketplace root itself and must never remove that registry.
    let cache = plugins_root(data)
        .join("cache")
        .join(&plugin.marketplace)
        .join(name);
    if plugin.marketplace != "local" && plugin.path.starts_with(&cache) {
        let _ = fs::remove_dir_all(cache);
    }
    write_map(&installed_file(data), &plugins)
}

pub fn marketplaces(data: &Path) -> BTreeMap<String, PathBuf> {
    read_map(&marketplaces_file(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_marketplace(root: &Path) {
        let plugin = root.join("demo-plugin");
        fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
        fs::create_dir_all(plugin.join("commands")).unwrap();
        fs::write(
            plugin.join(".claude-plugin/plugin.json"),
            r#"{"name": "demo", "description": "Demo plugin"}"#,
        )
        .unwrap();
        fs::write(
            plugin.join("commands/shipit.md"),
            "---\ndescription: Ship it\n---\nShip $ARGUMENTS now.",
        )
        .unwrap();
        fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        fs::write(
            root.join(".claude-plugin/marketplace.json"),
            r#"{"name": "fixture", "plugins": [
                {"name": "demo", "source": "./demo-plugin", "description": "Demo plugin"}
            ]}"#,
        )
        .unwrap();
    }

    #[test]
    fn add_install_enable_disable_remove_round_trip() {
        let data = std::env::temp_dir().join(format!("haarness-plugins-{}", uuid::Uuid::now_v7()));
        let market = std::env::temp_dir().join(format!("haarness-market-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&market).unwrap();
        fixture_marketplace(&market);

        let name = marketplace_add(&data, market.to_str().unwrap()).unwrap();
        assert_eq!(name, "fixture");
        assert!(marketplaces(&data).contains_key("fixture"));

        let plugin = install(&data, "demo@fixture").unwrap();
        assert!(plugin.enabled);
        assert!(plugin.path.join("commands/shipit.md").exists());
        assert_eq!(enabled_roots(&data).len(), 1);
        assert_eq!(update(&data, "demo").unwrap().marketplace, "fixture");

        // Bare name resolves when only one marketplace exists.
        install(&data, "demo").unwrap();

        set_enabled(&data, "demo", false).unwrap();
        assert!(enabled_roots(&data).is_empty());
        assert!(!update(&data, "demo").unwrap().enabled);
        assert!(enabled_roots(&data).is_empty());
        set_enabled(&data, "demo", true).unwrap();
        remove(&data, "demo").unwrap();
        assert!(installed(&data).is_empty());

        let _ = fs::remove_dir_all(&data);
        let _ = fs::remove_dir_all(&market);
    }

    #[test]
    fn marketplace_root_sources_materialize_without_widening_other_paths() {
        assert!(safe_relative_plugin_source("."));
        assert!(safe_relative_plugin_source("./"));
        assert!(!safe_relative("."));
        assert!(!safe_relative("./"));
        for unsafe_path in [
            "",
            "..",
            "../outside",
            "./../outside",
            "/absolute",
            "C:drive",
        ] {
            assert!(!safe_relative_plugin_source(unsafe_path), "{unsafe_path}");
        }

        let root =
            std::env::temp_dir().join(format!("haarness-root-source-{}", uuid::Uuid::now_v7()));
        let data = root.join("data");
        let market = root.join("market");
        fs::create_dir_all(market.join(".claude-plugin")).unwrap();
        fs::create_dir_all(market.join("skills/root-source")).unwrap();
        fs::create_dir_all(market.join(".git")).unwrap();
        fs::write(
            market.join(".claude-plugin/marketplace.json"),
            r#"{"name":"root-market","plugins":[{"name":"root-kit","source":"./"}]}"#,
        )
        .unwrap();
        fs::write(
            market.join(".claude-plugin/plugin.json"),
            r#"{"name":"root-kit"}"#,
        )
        .unwrap();
        fs::write(
            market.join("skills/root-source/SKILL.md"),
            "# root source\n",
        )
        .unwrap();
        fs::write(market.join(".git/sentinel"), "not plugin content\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("SKILL.md", market.join("skills/root-source/ALIAS.md")).unwrap();

        marketplace_add(&data, market.to_str().unwrap()).unwrap();
        let plugin = install(&data, "root-kit@root-market").unwrap();
        assert!(
            plugin
                .path
                .starts_with(data.join("plugins/cache/root-market/root-kit"))
        );
        assert!(plugin.path.join("skills/root-source/SKILL.md").exists());
        assert!(!plugin.path.join(".git").exists());
        #[cfg(unix)]
        assert_eq!(
            fs::read_link(plugin.path.join("skills/root-source/ALIAS.md")).unwrap(),
            PathBuf::from("SKILL.md")
        );

        remove(&data, "root-kit").unwrap();
        assert!(market.join(".claude-plugin/marketplace.json").exists());
        assert!(market.join("skills/root-source/SKILL.md").exists());

        fs::write(
            market.join(".claude-plugin/marketplace.json"),
            r#"{"name":"root-market","plugins":[{"name":"unsafe","source":""}]}"#,
        )
        .unwrap();
        assert!(
            install(&data, "unsafe@root-market")
                .unwrap_err()
                .to_string()
                .contains("unsafe relative source path")
        );

        fs::write(
            market.join(".claude-plugin/marketplace.json"),
            r#"{"name":"root-market","plugins":[{"name":"unsafe-subdir","source":{"source":"git-subdir","url":"unused","path":"./"}}]}"#,
        )
        .unwrap();
        assert!(
            install(&data, "unsafe-subdir@root-market")
                .unwrap_err()
                .to_string()
                .contains("unsafe git subdirectory path")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn marketplace_root_source_rejects_an_external_symlink() {
        let root =
            std::env::temp_dir().join(format!("haarness-root-symlink-{}", uuid::Uuid::now_v7()));
        let data = root.join("data");
        let market = root.join("market");
        let outside = root.join("outside");
        fs::create_dir_all(market.join(".claude-plugin")).unwrap();
        fs::create_dir_all(market.join("skills")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "outside\n").unwrap();
        fs::write(
            market.join(".claude-plugin/marketplace.json"),
            r#"{"name":"root-market","plugins":[{"name":"root-kit","source":"."}]}"#,
        )
        .unwrap();
        fs::write(
            market.join(".claude-plugin/plugin.json"),
            r#"{"name":"root-kit"}"#,
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside, market.join("skills/escape")).unwrap();

        marketplace_add(&data, market.to_str().unwrap()).unwrap();
        assert!(
            install(&data, "root-kit@root-market")
                .unwrap_err()
                .to_string()
                .contains("plugin symlink escapes its root")
        );
        assert!(installed(&data).is_empty());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ignores_claude_code_plugin_records_and_keeps_haarness_plugins() {
        let root = std::env::temp_dir().join(format!("haarness-claude-{}", uuid::Uuid::now_v7()));
        let home = root.join("home");
        let workspace = root.join("workspace");
        let plugin = home.join(".claude/plugins/cache/demo/1.0.0");
        fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
        fs::create_dir_all(plugin.join("commands")).unwrap();
        fs::create_dir_all(plugin.join("skills/demo")).unwrap();
        fs::create_dir_all(home.join(".claude/plugins")).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        fs::write(
            home.join(".claude/settings.json"),
            r#"{"enabledPlugins":{"demo@official":true}}"#,
        )
        .unwrap();
        fs::write(
            home.join(".claude/plugins/installed_plugins.json"),
            serde_json::json!({
                "plugins": {
                    "demo@official": [{"scope":"user","installPath":plugin,"version":"1.0.0"}]
                }
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            plugin.join(".claude-plugin/plugin.json"),
            r#"{"name":"demo","description":"Claude fixture"}"#,
        )
        .unwrap();

        fs::create_dir_all(workspace.join(".claude")).unwrap();
        fs::write(
            workspace.join(".claude/settings.json"),
            r#"{"enabledPlugins":{"demo@official":true}}"#,
        )
        .unwrap();
        assert!(
            all_enabled_roots(&root.join("data"), &root.join("config"), &workspace, true)
                .is_empty()
        );
        assert!(
            plugin_infos(&root.join("data"), &root.join("config"), &workspace, true).is_empty()
        );

        let own = root.join("config/plugins/local/own");
        fs::create_dir_all(own.join(".claude-plugin")).unwrap();
        fs::write(own.join(".claude-plugin/plugin.json"), r#"{"name":"own"}"#).unwrap();
        assert_eq!(
            all_enabled_roots(&root.join("data"), &root.join("config"), &workspace, true),
            vec![own]
        );
        assert_eq!(
            plugin_infos(&root.join("data"), &root.join("config"), &workspace, true)[0].name,
            "own"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn installs_a_local_plugin_directory_without_a_marketplace() {
        let root = std::env::temp_dir().join(format!("haarness-local-{}", uuid::Uuid::now_v7()));
        let data = root.join("data");
        let plugin = root.join("source");
        fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
        fs::write(
            plugin.join(".claude-plugin/plugin.json"),
            r#"{"name":"local-kit","description":"Local fixture"}"#,
        )
        .unwrap();

        let installed = install(&data, plugin.to_str().unwrap()).unwrap();
        assert_eq!(installed.marketplace, "local");
        assert_eq!(installed.path, plugin.canonicalize().unwrap());
        assert!(installed.enabled);
        remove(&data, "local-kit").unwrap();
        assert!(
            plugin.exists(),
            "external plugin sources must not be deleted"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_missing_local_folder_says_so() {
        let root = std::env::temp_dir().join(format!("haarness-missing-{}", uuid::Uuid::now_v7()));
        let data = root.join("data");
        let missing = root.join("does-not-exist/my-plugin");
        let missing = missing.to_str().unwrap();
        assert_eq!(
            install(&data, missing).unwrap_err().to_string(),
            format!("{missing} does not exist")
        );
        assert_eq!(
            marketplace_add(&data, missing).unwrap_err().to_string(),
            format!("{missing} does not exist")
        );
        assert_eq!(
            install(&data, "./not-here").unwrap_err().to_string(),
            "./not-here does not exist"
        );
        assert_eq!(
            install(&data, "kit").unwrap_err().to_string(),
            "kit is not a folder on this computer, and no plugin marketplace has been added to install it from",
            "a plain name is still a marketplace plugin"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn installs_a_git_subdirectory_source_and_rejects_relative_escape() {
        let root = std::env::temp_dir().join(format!("haarness-subdir-{}", uuid::Uuid::now_v7()));
        let data = root.join("data");
        let repo = root.join("repo");
        let plugin = repo.join("plugins/subdir-kit");
        fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
        fs::write(
            plugin.join(".claude-plugin/plugin.json"),
            r#"{"name":"subdir-kit"}"#,
        )
        .unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["add", "."],
            vec![
                "-c",
                "user.name=Haarness Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "fixture",
            ],
        ] {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(&repo)
                    .status()
                    .unwrap()
                    .success()
            );
        }

        let market = root.join("market");
        fs::create_dir_all(market.join(".claude-plugin")).unwrap();
        fs::write(
            market.join(".claude-plugin/marketplace.json"),
            serde_json::json!({
                "name": "fixture",
                "plugins": [{
                    "name": "subdir-kit",
                    "source": {
                        "source": "git-subdir",
                        "url": format!("file://{}", repo.display()),
                        "path": "plugins/subdir-kit"
                    }
                }]
            })
            .to_string(),
        )
        .unwrap();
        marketplace_add(&data, market.to_str().unwrap()).unwrap();
        let installed = install(&data, "subdir-kit@fixture").unwrap();
        assert!(installed.path.ends_with("plugins/subdir-kit"));

        fs::write(
            market.join(".claude-plugin/marketplace.json"),
            r#"{"name":"fixture","plugins":[{"name":"escape","source":"../repo"}]}"#,
        )
        .unwrap();
        assert!(
            install(&data, "escape@fixture")
                .unwrap_err()
                .to_string()
                .contains("unsafe relative source path")
        );

        fs::remove_dir_all(root).unwrap();
    }
}
