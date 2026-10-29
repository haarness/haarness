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
//! - `marketplace-sources.json` — name → where a Git marketplace's clone came
//!   from, its commit and whether it follows new commits (beside the first
//!   file, which keeps the shape older versions read)
//! - `installed.json`    — name → { marketplace, path, enabled }
//! - `marketplaces/<name>/`, `cache/<name>/` — cloned content

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

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
    /// A digest of the files Haarness installed, so an update can tell local
    /// edits. Records from before it was kept have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// Why the automatic update of it waits ("has local edits", "asks for
    /// more: new hooks").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<String>,
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

/// Written beside the file and renamed over it, so a reader never sees half
/// of it.
fn write_map<T: Serialize>(path: &Path, map: &BTreeMap<String, T>) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let leaf = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state");
    let temporary = parent.join(format!(".{leaf}.{}.tmp", uuid::Uuid::now_v7()));
    fs::write(&temporary, serde_json::to_string_pretty(map)?)?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    Ok(())
}

fn marketplaces_file(data: &Path) -> PathBuf {
    plugins_root(data).join("marketplaces.json")
}
fn installed_file(data: &Path) -> PathBuf {
    plugins_root(data).join("installed.json")
}
fn sources_file(data: &Path) -> PathBuf {
    plugins_root(data).join("marketplace-sources.json")
}

/// Where a Git marketplace's clone came from. Kept in its own file so
/// `marketplaces.json` stays as older versions read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceSource {
    /// The Git address as registered (after `owner/repo` became a URL).
    pub source: String,
    /// Fetched again by the background refresh.
    #[serde(default)]
    pub auto_update: bool,
    /// The commit the clone holds.
    #[serde(default)]
    pub commit: Option<String>,
    /// When the clone was last replaced (RFC 3339).
    #[serde(default)]
    pub updated_at: Option<String>,
}

pub fn marketplace_sources(data: &Path) -> BTreeMap<String, MarketplaceSource> {
    read_map(&sources_file(data))
}

/// The folder Haarness clones a Git marketplace into. A marketplace whose
/// registered path is anywhere else is a folder on this computer.
fn owned_clone(data: &Path, name: &str) -> PathBuf {
    plugins_root(data).join("marketplaces").join(name)
}

fn is_owned_clone(data: &Path, name: &str, root: &Path) -> bool {
    let clone = owned_clone(data, name);
    root == clone
        || root
            .canonicalize()
            .ok()
            .zip(clone.canonicalize().ok())
            .is_some_and(|(root, clone)| root == clone)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
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
    // The system folder's local plugins, which only its own maintainers change,
    // each left out when a link leads it into the project.
    roots.extend(system_plugin_roots(workspace));
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

/// The system folder's enabled local plugins, without one a link leads into
/// the project: its hooks and MCP servers would run as the system folder's.
fn system_plugin_roots(workspace: &Path) -> Vec<PathBuf> {
    crate::layout::SystemFolder::for_workspace(workspace)
        .map(|base| system_plugin_roots_in(&base))
        .unwrap_or_default()
}

pub(crate) fn system_plugin_roots_in(base: &crate::layout::SystemFolder) -> Vec<PathBuf> {
    base.entry("plugins/local")
        .map(|local| local_plugin_roots(&local))
        .unwrap_or_default()
        .into_iter()
        .filter(|root| !base.excluded(root))
        .collect()
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
            // Haarness never writes into the system folder: its plugins are
            // switched off or removed there.
            managed: scope != "system",
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
    if let Some(base) = crate::layout::SystemFolder::for_workspace(workspace)
        && let Some(local) = base.entry("plugins/local")
    {
        result.extend(
            local_plugin_infos(&local, "system")
                .into_iter()
                .filter(|info| !base.excluded(Path::new(&info.path))),
        );
    }
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
            "marketplace_update" => {
                let target = request.target.trim();
                let refreshed = if target.is_empty() {
                    marketplace_update_all(&self.data, false)
                } else {
                    vec![marketplace_update(&self.data, target)?]
                };
                let failed = refreshed
                    .iter()
                    .filter_map(|refresh| {
                        refresh
                            .error
                            .as_ref()
                            .map(|error| format!("{}: {error}", refresh.name))
                    })
                    .collect::<Vec<_>>();
                if !failed.is_empty() {
                    bail!("{}", failed.join("; "));
                }
            }
            "marketplace_auto_update" => {
                // `name`, `name:on` or `name:off`; a marketplace name never holds `:`.
                let (name, enabled) = match request.target.trim().rsplit_once(':') {
                    Some((name, "on")) => (name, true),
                    Some((name, "off")) => (name, false),
                    Some(_) => bail!("use <marketplace>, <marketplace>:on or <marketplace>:off"),
                    None => (request.target.trim(), true),
                };
                marketplace_auto_update(&self.data, name, enabled)?;
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
                "unknown plugin action {action}; use install, update, enable, disable, remove, marketplace, marketplace_update or marketplace_auto_update"
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

/// A marketplace or plugin name becomes one folder under the plugins folder.
/// A `:` is refused on every system: on Windows `C:` or `C:x` names a drive,
/// and joining it replaces the whole path, so a marketplace's text could have
/// pointed an install, or the removal before one, at another folder.
fn safe_name(name: &str, kind: &str) -> Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', ':'])
        || name.chars().any(char::is_control)
    {
        bail!("invalid {kind} name {name:?}: use one folder name without /, \\ or :");
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

/// The revision a git plugin source pins: `ref` is a branch or tag, `sha` an
/// exact commit that wins over `ref`, as in Claude Code's marketplace format.
#[derive(Default)]
struct GitRevision<'a> {
    reference: Option<&'a str>,
    sha: Option<&'a str>,
}

impl<'a> GitRevision<'a> {
    fn of(source: &'a serde_json::Map<String, serde_json::Value>) -> Self {
        let field = |key| source.get(key).and_then(serde_json::Value::as_str);
        Self {
            reference: field("ref"),
            sha: field("sha"),
        }
    }
}

/// Marketplace text names the repository and revision, so neither may be
/// read by git as an option, and fetching must never stop to ask for a password.
fn git<S: AsRef<std::ffi::OsStr>>(args: impl IntoIterator<Item = S>) -> Result<()> {
    git_output(args).map(drop)
}

/// [`git`], returning what it printed. A clone or fetch gets ten minutes, a
/// question to a remote one, anything else thirty seconds.
fn git_output<S: AsRef<std::ffi::OsStr>>(args: impl IntoIterator<Item = S>) -> Result<String> {
    let args = args
        .into_iter()
        .map(|arg| arg.as_ref().to_os_string())
        .collect::<Vec<_>>();
    let limit = if args.iter().any(|arg| arg == "clone" || arg == "fetch") {
        Duration::from_secs(600)
    } else if args.iter().any(|arg| arg == "ls-remote") {
        Duration::from_secs(60)
    } else {
        Duration::from_secs(30)
    };
    git_output_within(&args, limit)
}

/// The `ssh` git runs: never asking anything, and giving up on a host that
/// does not answer. A command the person set keeps its own options.
fn ssh_command() -> Option<String> {
    const OPTIONS: &str = "-o BatchMode=yes -o ConnectTimeout=15";
    match std::env::var("GIT_SSH_COMMAND") {
        Ok(command) if !command.trim().is_empty() => Some(format!("{command} {OPTIONS}")),
        // `GIT_SSH` names a program that takes no options; leave it alone.
        _ if std::env::var_os("GIT_SSH").is_some() => None,
        _ => Some(format!("ssh {OPTIONS}")),
    }
}

/// Runs git without a terminal: no prompt, no input (a background server
/// that reads the terminal is stopped by it), a transfer that stalls for 30
/// seconds fails, and the whole run is killed, with what it started, once
/// `limit` has passed. Output goes to files, so a helper git leaves running
/// cannot hold this up.
fn git_output_within(args: &[std::ffi::OsString], limit: Duration) -> Result<String> {
    let scratch = std::env::temp_dir();
    let nonce = uuid::Uuid::now_v7();
    let out_path = scratch.join(format!("haarness-git-{nonce}.out"));
    let err_path = scratch.join(format!("haarness-git-{nonce}.err"));
    let result = (|| -> Result<String> {
        let mut command = Command::new("git");
        command
            .args([
                "-c",
                "http.lowSpeedLimit=1000",
                "-c",
                "http.lowSpeedTime=30",
            ])
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ALLOW_PROTOCOL", "file:git:http:https:ssh")
            .stdin(std::process::Stdio::null())
            .stdout(fs::File::create(&out_path)?)
            .stderr(fs::File::create(&err_path)?);
        if let Some(ssh) = ssh_command() {
            command.env("GIT_SSH_COMMAND", ssh);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Its own group, so what it starts (ssh, a remote helper) is
            // stopped with it.
            command.process_group(0);
        }
        let mut child = command.spawn().context("running git")?;
        let deadline = std::time::Instant::now() + limit;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if std::time::Instant::now() >= deadline {
                #[cfg(unix)]
                if let Ok(pid) = i32::try_from(child.id()) {
                    let _ = nix::sys::signal::killpg(
                        nix::unistd::Pid::from_raw(pid),
                        nix::sys::signal::Signal::SIGKILL,
                    );
                }
                let _ = child.kill();
                let _ = child.wait();
                bail!(
                    "git did not finish within {} seconds, so it was stopped",
                    limit.as_secs()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        if !status.success() {
            let stderr = fs::read(&err_path).unwrap_or_default();
            let stderr = String::from_utf8_lossy(&stderr);
            // Git quotes the remote's address, which can carry a token.
            bail!(
                "{}",
                haarness_core::redact::urls_in(
                    stderr
                        .lines()
                        .rev()
                        .find(|line| !line.trim().is_empty())
                        .unwrap_or("git failed")
                        .trim()
                )
            );
        }
        Ok(String::from_utf8_lossy(&fs::read(&out_path)?).into_owned())
    })();
    let _ = fs::remove_file(&out_path);
    let _ = fs::remove_file(&err_path);
    result
}

/// The commit a checkout holds.
fn head_commit(checkout: &Path) -> Result<String> {
    let checkout = checkout.to_string_lossy();
    let head = git_output(["-C", checkout.as_ref(), "rev-parse", "HEAD"])?;
    Ok(head.trim().to_owned())
}

/// Where a clone was cloned from, for marketplaces registered before their
/// source was recorded.
fn origin_url(checkout: &Path) -> Option<String> {
    let checkout = checkout.to_string_lossy();
    git_output(["-C", checkout.as_ref(), "remote", "get-url", "origin"])
        .ok()
        .map(|url| url.trim().to_owned())
        .filter(|url| !url.is_empty())
}

/// The commit `reference` (the default branch when `None`) names in the
/// repository at `url`, asked without cloning. A name is looked up as git's
/// `--branch` reads it: the branch, else the tag (the commit an annotated tag
/// points to), matched by full name, so `v1` never means `feature/v1`.
fn remote_commit(url: &str, reference: Option<&str>) -> Result<String> {
    checked_git_value(url, "URL")?;
    let reference = reference
        .map(|reference| checked_git_value(reference, "ref"))
        .transpose()?;
    let shown = haarness_core::redact::url(url);
    let wanted = match reference {
        None => vec!["HEAD".to_owned()],
        Some(full) if full.starts_with("refs/") => vec![format!("{full}^{{}}"), full.to_owned()],
        Some(name) => vec![
            format!("refs/heads/{name}"),
            format!("refs/tags/{name}^{{}}"),
            format!("refs/tags/{name}"),
        ],
    };
    let mut args = vec!["ls-remote".to_owned(), "--".to_owned(), url.to_owned()];
    // The peeled `^{}` name is asked for by itself: git lists it only then.
    args.extend(wanted.iter().cloned());
    let listed = git_output(&args).with_context(|| format!("could not reach {shown}"))?;
    let refs = listed
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(commit, name)| (name.trim(), commit.trim()))
        .collect::<BTreeMap<_, _>>();
    wanted
        .iter()
        .find_map(|name| refs.get(name.as_str()))
        .map(|commit| (*commit).to_owned())
        .with_context(|| format!("{shown} has no {}", reference.unwrap_or("HEAD")))
}

fn checked_git_value<'a>(value: &'a str, what: &str) -> Result<&'a str> {
    if value.is_empty()
        || value.starts_with('-')
        || value.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        bail!("invalid git {what} {value:?}");
    }
    Ok(value)
}

fn git_clone(url: &str, destination: &Path) -> Result<()> {
    git_checkout(url, &GitRevision::default(), destination)
}

fn git_checkout(url: &str, revision: &GitRevision<'_>, destination: &Path) -> Result<()> {
    checked_git_value(url, "URL")?;
    let reference = revision
        .reference
        .map(|reference| checked_git_value(reference, "ref"))
        .transpose()?;
    if let Some(sha) = revision.sha
        && !(sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        bail!("plugin source sha must be a full 40-character commit SHA, not {sha:?}");
    }
    let parent = destination.parent().unwrap_or(destination);
    fs::create_dir_all(parent)?;
    let leaf = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("plugin");
    // Messages name the source with any user name, password or secret query
    // parameter hidden; git itself still gets the address as written.
    let shown = haarness_core::redact::url(url);
    let nonce = uuid::Uuid::now_v7();
    let staging = parent.join(format!(".{leaf}.stage-{nonce}"));
    let backup = parent.join(format!(".{leaf}.backup-{nonce}"));
    let fetched = (|| -> Result<()> {
        let staged = staging.to_string_lossy();
        let staged = staged.as_ref();
        let Some(sha) = revision.sha else {
            let mut args = vec!["clone", "-q", "--depth", "1"];
            if let Some(reference) = reference {
                args.extend(["--branch", reference]);
            }
            args.extend(["--", url, staged]);
            return git(args);
        };
        let sha = sha.to_ascii_lowercase();
        git(["-c", "init.defaultBranch=main", "init", "-q", staged])?;
        // Most hosts serve a reachable commit by its id. Others only serve
        // refs, so fall back to the named ref's (or default branch's) history.
        let by_id = git(["-C", staged, "fetch", "-q", "--depth", "1", "--", url, &sha]);
        if by_id.is_err() {
            git([
                "-C",
                staged,
                "fetch",
                "-q",
                "--",
                url,
                reference.unwrap_or("HEAD"),
            ])
            .with_context(|| format!("commit {sha} is not available from {shown}"))?;
        }
        git(["-C", staged, "checkout", "-q", "--detach", &sha])
            .with_context(|| format!("commit {sha} is not available from {shown}"))
    })();
    if let Err(error) = fetched {
        let _ = fs::remove_dir_all(&staging);
        return Err(error.context(format!("git clone failed for {shown}")));
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
                    bail!(
                        "a link in the plugin leads outside its folder: {}",
                        path.display()
                    );
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

/// Serializes changes to registered marketplaces across Haarness processes:
/// a refresh swapping a clone, a registration, a setting. Never held while
/// git runs.
fn marketplaces_lock(data: &Path) -> Result<fs::File> {
    fs::create_dir_all(data)?;
    crate::desktop::editor_lock(data, "plugin-marketplaces")
}

/// Serializes every change to `installed.json` across Haarness processes,
/// held only while it is read, changed and written (and a fetched plugin
/// moved into place), never while git runs.
fn installed_lock(data: &Path) -> Result<fs::File> {
    fs::create_dir_all(data)?;
    crate::desktop::editor_lock(data, "plugins-installed")
}

/// Reads `installed.json`, lets `change` edit it and writes it back, all
/// under [`installed_lock`]. Nothing is written when `change` fails.
fn change_installed<T>(
    data: &Path,
    change: impl FnOnce(&mut BTreeMap<String, InstalledPlugin>) -> Result<T>,
) -> Result<T> {
    let _lock = installed_lock(data)?;
    let mut plugins = installed(data);
    let value = change(&mut plugins)?;
    write_map(&installed_file(data), &plugins)?;
    Ok(value)
}

/// Moves `staging` to `destination`, keeping what was there until the move
/// has worked.
fn swap_into(staging: &Path, destination: &Path) -> Result<()> {
    let parent = destination.parent().unwrap_or(destination);
    let leaf = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("folder");
    let backup = parent.join(format!(".{leaf}.previous-{}", uuid::Uuid::now_v7()));
    let had = destination.exists();
    if had {
        fs::rename(destination, &backup)?;
    }
    if let Err(error) = fs::rename(staging, destination) {
        if had {
            let _ = fs::rename(&backup, destination);
        }
        return Err(error.into());
    }
    if had {
        let _ = fs::remove_dir_all(&backup);
    }
    Ok(())
}

/// Register a marketplace from a git URL or a local directory. A Git one
/// follows new commits of its repository from then on. The clone is made
/// before the registrations are locked, so a slow host holds nobody up.
pub fn marketplace_add(data: &Path, source: &str) -> Result<String> {
    let local = local_path(source).unwrap_or_else(|| PathBuf::from(source));
    if local.exists() {
        let root = local.canonicalize()?;
        let manifest = manifest_of(&root)?;
        safe_name(&manifest.name, "marketplace")?;
        let _lock = marketplaces_lock(data)?;
        let mut marketplaces = marketplaces(data);
        let mut sources = marketplace_sources(data);
        // A folder is read as it is; nothing to fetch.
        sources.remove(&manifest.name);
        marketplaces.insert(manifest.name.clone(), root);
        write_map(&marketplaces_file(data), &marketplaces)?;
        write_map(&sources_file(data), &sources)?;
        return Ok(manifest.name);
    }
    if local_path(source).is_some() {
        bail!("{source} does not exist");
    }
    // Normalize owner/repo shorthand to a GitHub URL.
    let url = git_url(source);
    let staging = plugins_root(data)
        .join("marketplaces")
        .join(format!(".add-{}", uuid::Uuid::now_v7()));
    let checked = git_clone(&url, &staging).and_then(|()| {
        let manifest = manifest_of(&staging)?;
        safe_name(&manifest.name, "marketplace")?;
        let commit = head_commit(&staging).ok();
        Ok((manifest.name, commit))
    });
    let (name, commit) = match checked {
        Ok(checked) => checked,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    let _lock = marketplaces_lock(data)?;
    let destination = owned_clone(data, &name);
    if let Err(error) = swap_into(&staging, &destination) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    let mut marketplaces = marketplaces(data);
    let mut sources = marketplace_sources(data);
    sources.insert(
        name.clone(),
        MarketplaceSource {
            source: url,
            auto_update: true,
            commit,
            updated_at: Some(now()),
        },
    );
    marketplaces.insert(name.clone(), destination);
    write_map(&marketplaces_file(data), &marketplaces)?;
    write_map(&sources_file(data), &sources)?;
    Ok(name)
}

/// What [`marketplace_update`] did to one marketplace.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceRefresh {
    pub name: String,
    /// `unchanged`, `updated`, `folder` (read as it is, so never fetched) or
    /// `failed` (with `error`; the clone it had is kept).
    pub status: &'static str,
    /// The commit the clone holds now, and the one it held before.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
    /// Installed plugins whose files changed and were updated.
    pub plugins: Vec<String>,
    /// Installed plugins the marketplace no longer lists, left as they are.
    pub unlisted: Vec<String>,
    /// Installed plugins whose new version waits for the person, with why.
    pub held: Vec<PluginHeld>,
    /// Installed plugins that could not be updated, with why.
    pub failed: Vec<PluginFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginFailure {
    pub plugin: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginHeld {
    pub plugin: String,
    pub reason: String,
}

impl MarketplaceRefresh {
    fn new(name: &str, status: &'static str) -> Self {
        Self {
            name: name.to_owned(),
            status,
            ..Self::default()
        }
    }

    /// Whether files a running server loads changed.
    pub fn changed(&self) -> bool {
        self.status == "updated" || !self.plugins.is_empty()
    }
}

/// The recorded source of a marketplace Haarness cloned, or, for one
/// registered before sources were recorded, its clone's `origin`.
fn source_of(data: &Path, name: &str) -> Result<MarketplaceSource> {
    if let Some(source) = marketplace_sources(data).remove(name) {
        return Ok(source);
    }
    let clone = owned_clone(data, name);
    let url = origin_url(&clone).with_context(|| {
        format!(
            "{name} does not say where it was cloned from; register it again with `haarness plugin marketplace add <source>`"
        )
    })?;
    Ok(MarketplaceSource {
        source: url,
        auto_update: false,
        commit: head_commit(&clone).ok(),
        updated_at: None,
    })
}

/// Changes one marketplace's recorded source as it is on disk now (or as
/// `found` says, for one with no record yet). Callers hold
/// [`marketplaces_lock`], so a setting changed meanwhile is kept.
fn edit_source(
    data: &Path,
    name: &str,
    found: &MarketplaceSource,
    edit: impl FnOnce(&mut MarketplaceSource),
) -> Result<MarketplaceSource> {
    let mut sources = marketplace_sources(data);
    let mut source = sources.get(name).cloned().unwrap_or_else(|| found.clone());
    edit(&mut source);
    if sources.get(name) != Some(&source) {
        sources.insert(name.to_owned(), source.clone());
        write_map(&sources_file(data), &sources)?;
    }
    Ok(source)
}

/// After a stop between the two renames of a swap the clone is missing and
/// its previous copy sits beside it: put that back. Copies left by a swap
/// that finished are removed. Callers hold [`marketplaces_lock`].
fn recover_clone(data: &Path, name: &str) -> Result<()> {
    let clone = owned_clone(data, name);
    let parent = plugins_root(data).join("marketplaces");
    let prefix = format!(".{name}.previous-");
    let mut previous = fs::read_dir(&parent)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|leaf| leaf.starts_with(&prefix))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    // Names end in a time-ordered id: the last is the newest.
    previous.sort();
    if !clone.exists()
        && let Some(newest) = previous.pop()
    {
        fs::rename(&newest, &clone)?;
    }
    for stale in previous {
        let _ = fs::remove_dir_all(stale);
    }
    Ok(())
}

/// Fetch a registered Git marketplace again when its repository has new
/// commits, then update each installed plugin of it whose files changed.
/// The new clone is made without holding any lock and replaces the old one
/// only once it is complete and still names itself the same; anything that
/// fails before that leaves the old clone in place.
pub fn marketplace_update(data: &Path, name: &str) -> Result<MarketplaceRefresh> {
    safe_name(name, "marketplace")?;
    let root = marketplaces(data)
        .remove(name)
        .with_context(|| format!("unknown marketplace {name}; add it first"))?;
    if !is_owned_clone(data, name, &root) {
        return Ok(MarketplaceRefresh::new(name, "folder"));
    }
    let clone = owned_clone(data, name);
    {
        let _lock = marketplaces_lock(data)?;
        recover_clone(data, name)?;
    }
    let found = source_of(data, name)?;
    let current = head_commit(&clone).ok();
    let remote = remote_commit(&found.source, None)?;
    // Unchanged only when the clone and the record both hold the remote's
    // commit; a record that disagrees is put right by fetching again.
    if current.as_deref() == Some(remote.as_str())
        && found
            .commit
            .as_deref()
            .is_none_or(|commit| commit == remote)
    {
        // A marketplace registered before sources were recorded keeps one now.
        let _lock = marketplaces_lock(data)?;
        edit_source(data, name, &found, |source| {
            source.commit.clone_from(&current);
        })?;
        return Ok(MarketplaceRefresh {
            commit: current,
            ..MarketplaceRefresh::new(name, "unchanged")
        });
    }
    let parent = plugins_root(data).join("marketplaces");
    let staging = parent.join(format!(".{name}.refresh-{}", uuid::Uuid::now_v7()));
    let fetched = git_clone(&found.source, &staging).and_then(|()| {
        let manifest = manifest_of(&staging)?;
        if manifest.name != name {
            bail!(
                "{name} now calls itself {:?}, so it was not updated; register it again under its new name",
                manifest.name
            );
        }
        Ok(manifest)
    });
    let manifest = match fetched {
        Ok(manifest) => manifest,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    let commit = head_commit(&staging).unwrap_or(remote);
    let before = {
        let _lock = marketplaces_lock(data)?;
        recover_clone(data, name)?;
        let now_current = head_commit(&clone).ok();
        if now_current != current {
            // Another Haarness replaced it meanwhile and updates its plugins.
            let _ = fs::remove_dir_all(&staging);
            return Ok(MarketplaceRefresh {
                commit: now_current,
                ..MarketplaceRefresh::new(name, "unchanged")
            });
        }
        let before = manifest_of(&clone)
            .map(|manifest| {
                manifest
                    .plugins
                    .into_iter()
                    .map(|plugin| (plugin.name, plugin.source))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let backup = parent.join(format!(".{name}.previous-{}", uuid::Uuid::now_v7()));
        let had = clone.exists();
        let swapped = (|| -> Result<()> {
            if had {
                fs::rename(&clone, &backup)?;
            }
            if let Err(error) = fs::rename(&staging, &clone) {
                if had {
                    let _ = fs::rename(&backup, &clone);
                }
                return Err(error.into());
            }
            let recorded = edit_source(data, name, &found, |source| {
                source.commit = Some(commit.clone());
                source.updated_at = Some(now());
            });
            if let Err(error) = recorded {
                let _ = fs::rename(&clone, &staging);
                if had {
                    let _ = fs::rename(&backup, &clone);
                }
                return Err(error);
            }
            Ok(())
        })();
        if let Err(error) = swapped {
            let _ = fs::remove_dir_all(&staging);
            return Err(error.context(format!("{name} was not updated")));
        }
        if had {
            let _ = fs::remove_dir_all(&backup);
        }
        before
    };
    let mut refresh = MarketplaceRefresh {
        commit: Some(commit),
        previous: current,
        ..MarketplaceRefresh::new(name, "updated")
    };
    for (plugin, record) in installed(data) {
        if record.marketplace != name {
            continue;
        }
        let Some(entry) = manifest.plugins.iter().find(|entry| entry.name == plugin) else {
            refresh.unlisted.push(plugin);
            continue;
        };
        match refresh_plugin(data, &clone, &plugin, &record, before.get(&plugin), entry) {
            Ok(Refreshed::Updated) => refresh.plugins.push(plugin),
            Ok(Refreshed::Unchanged) => {}
            Ok(Refreshed::Held(reason)) => refresh.held.push(PluginHeld { plugin, reason }),
            Err(error) => refresh.failed.push(PluginFailure {
                plugin,
                error: format!("{error:#}"),
            }),
        }
    }
    Ok(refresh)
}

/// Every registered marketplace, or only those that follow their
/// repository; one that fails is reported and the others still run.
pub fn marketplace_update_all(data: &Path, only_followed: bool) -> Vec<MarketplaceRefresh> {
    let followed = marketplace_sources(data);
    marketplaces(data)
        .into_iter()
        .filter(|(name, root)| {
            is_owned_clone(data, name, root)
                && (!only_followed || followed.get(name).is_some_and(|source| source.auto_update))
        })
        .map(|(name, _)| {
            marketplace_update(data, &name).unwrap_or_else(|error| MarketplaceRefresh {
                error: Some(format!("{error:#}")),
                ..MarketplaceRefresh::new(&name, "failed")
            })
        })
        .collect()
}

/// What refreshing one installed plugin did.
enum Refreshed {
    Updated,
    Unchanged,
    /// Its new version waits for the person: local edits, or it asks to run
    /// more than the installed one.
    Held(String),
}

/// The automatic update of one installed plugin after its marketplace was
/// fetched again. It never replaces local edits, and a version that would
/// add hooks, MCP servers or programs waits for `plugin update`; either way
/// the record says why.
fn refresh_plugin(
    data: &Path,
    clone: &Path,
    plugin: &str,
    record: &InstalledPlugin,
    before: Option<&serde_json::Value>,
    entry: &MarketplacePlugin,
) -> Result<Refreshed> {
    let outcome = (|| -> Result<Refreshed> {
        if !plugin_may_have_changed(data, plugin, record, before, entry)? {
            return Ok(Refreshed::Unchanged);
        }
        if locally_modified(record)? {
            return Ok(Refreshed::Held("has local edits".into()));
        }
        let staged = stage_plugin(data, &record.marketplace, plugin, clone, entry)?;
        let installed_tree = record
            .path
            .exists()
            .then(|| plugin_tree_digest(&record.path));
        if let Some(Ok(digest)) = &installed_tree
            && plugin_tree_digest(&staged.plugin()).ok().as_ref() == Some(digest)
        {
            staged.discard();
            return Ok(Refreshed::Unchanged);
        }
        if record.path.exists() {
            let more = asks_for_more(&record.path, &staged.plugin());
            if !more.is_empty() {
                staged.discard();
                return Ok(Refreshed::Held(format!(
                    "asks for more: {}",
                    more.join(", ")
                )));
            }
        }
        install_staged(
            data,
            plugin,
            &record.marketplace,
            staged,
            &entry.description,
            true,
        )?;
        Ok(Refreshed::Updated)
    })()?;
    // Say why it waits, or that nothing does any more.
    let held = match &outcome {
        Refreshed::Held(reason) => Some(reason.clone()),
        _ => None,
    };
    if !matches!(outcome, Refreshed::Updated) && record.held != held {
        change_installed(data, |plugins| {
            if let Some(current) = plugins.get_mut(plugin) {
                current.held.clone_from(&held);
            }
            Ok(())
        })?;
    }
    Ok(outcome)
}

/// A cheap first look: whether the files `update` would install for
/// `plugin` may differ from the installed ones. A plugin inside the
/// marketplace compares its folders; one fetched from its own repository may
/// have changed when its entry changed or, when it follows a branch, that
/// branch moved.
fn plugin_may_have_changed(
    data: &Path,
    plugin: &str,
    record: &InstalledPlugin,
    before: Option<&serde_json::Value>,
    entry: &MarketplacePlugin,
) -> Result<bool> {
    if !record.path.exists() || before != Some(&entry.source) {
        return Ok(true);
    }
    let cache = plugins_root(data)
        .join("cache")
        .join(&record.marketplace)
        .join(plugin);
    match &entry.source {
        // Compared once fetched: copying a folder costs little.
        serde_json::Value::String(source) if !is_git_source(source) => Ok(true),
        serde_json::Value::String(url) => {
            Ok(head_commit(&cache).ok() != Some(remote_commit(url, None)?))
        }
        serde_json::Value::Object(object) => {
            let revision = GitRevision::of(object);
            if revision.sha.is_some() {
                // Pinned to one commit, which the entry did not change.
                return Ok(false);
            }
            let Some(url) = git_source_url(object) else {
                return Ok(false);
            };
            Ok(head_commit(&cache).ok() != Some(remote_commit(&url, revision.reference)?))
        }
        _ => Ok(false),
    }
}

fn is_git_source(source: &str) -> bool {
    source.contains("://")
        || source.starts_with("git@")
        || source.to_ascii_lowercase().ends_with(".git")
}

fn git_source_url(object: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    match object
        .get("source")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("github")
    {
        "github" => object
            .get("repo")
            .and_then(serde_json::Value::as_str)
            .map(git_url),
        "url" | "git-subdir" => object
            .get("url")
            .and_then(serde_json::Value::as_str)
            .map(git_url),
        _ => None,
    }
}

/// Whether an installed plugin's files differ from what Haarness installed.
/// A record from before digests were kept cannot tell, and counts as not.
fn locally_modified(record: &InstalledPlugin) -> Result<bool> {
    match &record.digest {
        Some(digest) if record.path.exists() => Ok(plugin_tree_digest(&record.path)? != *digest),
        _ => Ok(false),
    }
}

/// What a new version of a plugin may run that the installed one may not:
/// hooks, MCP servers, or programs (files it may execute) that are new or
/// changed.
fn asks_for_more(installed: &Path, new: &Path) -> Vec<&'static str> {
    let mut more = Vec::new();
    let (old_hooks, new_hooks) = (plugin_hooks(installed), plugin_hooks(new));
    if new_hooks != old_hooks && !new_hooks.is_empty() {
        more.push(if old_hooks.is_empty() {
            "new hooks"
        } else {
            "changed hooks"
        });
    }
    let (old_servers, new_servers) = (mcp_servers(installed), mcp_servers(new));
    if new_servers
        .keys()
        .any(|server| !old_servers.contains_key(server))
    {
        more.push("new MCP servers");
    } else if new_servers
        .iter()
        .any(|(server, config)| old_servers.get(server) != Some(config))
    {
        more.push("changed MCP servers");
    }
    let old_programs = programs(installed);
    if programs(new)
        .iter()
        .any(|program| !old_programs.contains(program))
    {
        more.push("new programs");
    }
    more
}

fn plugin_manifest(root: &Path) -> serde_json::Value {
    fs::read_to_string(root.join(".claude-plugin/plugin.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(serde_json::Value::Null)
}

/// A JSON file of the plugin as text when it does not parse, so a broken
/// file still counts as changed.
fn plugin_json(root: &Path, relative: &str) -> Option<serde_json::Value> {
    if !safe_relative(relative) {
        return None;
    }
    let text = fs::read_to_string(root.join(relative)).ok()?;
    Some(serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text)))
}

/// Every hook definition a plugin brings: `hooks/hooks.json` and the
/// manifest's `hooks` (inline, or the file it names).
fn plugin_hooks(root: &Path) -> Vec<serde_json::Value> {
    let mut hooks = Vec::new();
    hooks.extend(plugin_json(root, "hooks/hooks.json"));
    match plugin_manifest(root).get("hooks") {
        Some(serde_json::Value::String(path)) => {
            hooks.extend(plugin_json(root, path.trim_start_matches("./")));
        }
        Some(serde_json::Value::Null) | None => {}
        Some(inline) => hooks.push(inline.clone()),
    }
    hooks
}

/// A plugin's MCP servers by name: `.mcp.json` and the manifest's
/// `mcpServers` (inline, or the file it names).
fn mcp_servers(root: &Path) -> BTreeMap<String, serde_json::Value> {
    let mut servers = BTreeMap::new();
    let mut add = |value: Option<serde_json::Value>| {
        let Some(value) = value else { return };
        let list = value.get("mcpServers").cloned().unwrap_or(value);
        match list {
            serde_json::Value::Object(list) => servers.extend(list),
            other => {
                servers.insert(String::new(), other);
            }
        }
    };
    add(plugin_json(root, ".mcp.json"));
    match plugin_manifest(root).get("mcpServers") {
        Some(serde_json::Value::String(path)) => {
            add(plugin_json(root, path.trim_start_matches("./")))
        }
        Some(serde_json::Value::Null) | None => {}
        Some(inline) => add(Some(inline.clone())),
    }
    servers
}

/// The files of a plugin someone may run directly: those marked executable.
fn programs(root: &Path) -> std::collections::BTreeSet<PathBuf> {
    let mut found = std::collections::BTreeSet::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                if entry.file_name() == ".git" {
                    continue;
                }
                let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
                    continue;
                };
                if metadata.is_dir() {
                    pending.push(entry.path());
                } else if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
                    if let Ok(relative) = entry.path().strip_prefix(root) {
                        found.insert(relative.to_path_buf());
                    }
                }
            }
        }
    }
    #[cfg(not(unix))]
    let _ = root;
    found
}

/// A digest of a plugin's files as Haarness installs them: names, kinds,
/// contents and link targets, without `.git` or the disabled marker.
fn plugin_tree_digest(root: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    fn walk(path: &Path, relative: &Path, hash: &mut Sha256) -> Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        hash.update(relative.to_string_lossy().as_bytes());
        if metadata.file_type().is_symlink() {
            hash.update([1]);
            hash.update(fs::read_link(path)?.to_string_lossy().as_bytes());
        } else if metadata.is_dir() {
            hash.update([2]);
            let mut children = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
            children.sort_by_key(fs::DirEntry::file_name);
            for child in children {
                let name = child.file_name();
                if name != ".git" && name != ".haarness-disabled" {
                    walk(&child.path(), &relative.join(name), hash)?;
                }
            }
        } else {
            hash.update([3]);
            let bytes = fs::read(path)?;
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
        hash.update([0]);
        Ok(())
    }
    let mut hash = Sha256::new();
    walk(root, Path::new(""), &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}

/// Turn the background refresh of a Git marketplace on or off.
pub fn marketplace_auto_update(
    data: &Path,
    name: &str,
    enabled: bool,
) -> Result<MarketplaceSource> {
    safe_name(name, "marketplace")?;
    let root = marketplaces(data)
        .remove(name)
        .with_context(|| format!("unknown marketplace {name}; add it first"))?;
    if !is_owned_clone(data, name, &root) {
        bail!(
            "{name} is a folder on this computer, which Haarness reads as it is; there is nothing to fetch"
        );
    }
    let found = source_of(data, name)?;
    let _lock = marketplaces_lock(data)?;
    edit_source(data, name, &found, |source| source.auto_update = enabled)
}

/// A source that names a folder on this computer: absolute, `~/…`, `./…` or
/// `../…`. Anything else may be a plugin name or an `owner/repo` shorthand.
fn local_path(spec: &str) -> Option<PathBuf> {
    if let Some(rest) = spec.strip_prefix("~/") {
        return haarness_protocol::home_dir().map(|home| home.join(rest));
    }
    let relative = [".", ".."].contains(&spec) || spec.starts_with("./") || spec.starts_with("../");
    (relative || Path::new(spec).is_absolute()).then(|| PathBuf::from(spec))
}

/// A marketplace plugin's files fetched into a folder beside its cache, not
/// installed yet.
struct StagedPlugin {
    /// What replaces the cache folder.
    root: PathBuf,
    /// The plugin inside it, for a `path` in a repository.
    subdir: Option<PathBuf>,
}

impl StagedPlugin {
    fn plugin(&self) -> PathBuf {
        self.subdir
            .as_ref()
            .map_or_else(|| self.root.clone(), |subdir| self.root.join(subdir))
    }

    fn discard(self) {
        let _ = fs::remove_dir_all(self.root);
    }
}

/// Fetches (git) or copies (a folder of the marketplace) a plugin's files
/// into a staging folder beside its cache. Nothing installed changes.
fn stage_plugin(
    data: &Path,
    market: &str,
    name: &str,
    root: &Path,
    entry: &MarketplacePlugin,
) -> Result<StagedPlugin> {
    let parent = plugins_root(data).join("cache").join(market);
    fs::create_dir_all(&parent)?;
    let staging = parent.join(format!(".{name}.fetch-{}", uuid::Uuid::now_v7()));
    let staged = (|| -> Result<StagedPlugin> {
        let subdir = match &entry.source {
            serde_json::Value::String(source) if is_git_source(source) => {
                git_clone(source, &staging)?;
                None
            }
            serde_json::Value::String(source) => {
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
                materialize_plugin_tree(&candidate, &staging)?;
                None
            }
            serde_json::Value::Object(object) => {
                let kind = object
                    .get("source")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("github");
                if !matches!(kind, "github" | "url" | "git-subdir") {
                    bail!("unsupported plugin source type {kind} for {name}");
                }
                let source = git_source_url(object).with_context(|| {
                    if kind == "github" {
                        "github plugin source needs repo"
                    } else {
                        "plugin source needs url"
                    }
                })?;
                // `git-subdir` requires a path; Anthropic's own marketplace
                // also gives some `url` sources one, so honour it there too.
                let subdir = match (kind, object.get("path").and_then(serde_json::Value::as_str)) {
                    ("git-subdir", None) => bail!("git-subdir plugin source needs path"),
                    ("git-subdir" | "url", Some(subdir)) => {
                        if !safe_relative(subdir) {
                            bail!("plugin {name} has an unsafe git subdirectory path");
                        }
                        Some(subdir)
                    }
                    _ => None,
                };
                git_checkout(&source, &GitRevision::of(object), &staging)?;
                match subdir {
                    Some(subdir) => {
                        let target = staging.join(subdir);
                        let checkout = staging.canonicalize()?;
                        let target = target.canonicalize().with_context(|| {
                            format!("plugin subdirectory {subdir} does not exist")
                        })?;
                        if !target.starts_with(&checkout) || !target.is_dir() {
                            bail!("plugin subdirectory must stay inside its repository");
                        }
                        Some(target.strip_prefix(&checkout)?.to_path_buf())
                    }
                    None => None,
                }
            }
            _ => bail!("unsupported plugin source for {name}"),
        };
        let staged = StagedPlugin {
            root: staging.clone(),
            subdir,
        };
        let plugin = staged.plugin();
        if !plugin.join(".claude-plugin/plugin.json").exists() && !plugin.join("commands").exists()
        {
            bail!("{name} does not look like a plugin (no .claude-plugin/plugin.json)");
        }
        Ok(staged)
    })();
    if staged.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    staged
}

/// Moves a staged plugin into its cache and records it, under
/// [`installed_lock`]. A plugin already installed keeps whether it is on;
/// with `existing`, one removed while it was fetched stays removed.
fn install_staged(
    data: &Path,
    name: &str,
    market: &str,
    staged: StagedPlugin,
    description: &str,
    existing: bool,
) -> Result<InstalledPlugin> {
    let cache = plugins_root(data).join("cache").join(market).join(name);
    let result = change_installed(data, |plugins| {
        let current = plugins.get(name);
        if existing && current.is_none_or(|current| current.marketplace != market) {
            bail!("{name} was removed while it was being updated, so it stays removed");
        }
        swap_into(&staged.root, &cache)?;
        let path = match &staged.subdir {
            Some(subdir) => cache.join(subdir).canonicalize()?,
            None => cache.clone(),
        };
        let plugin = InstalledPlugin {
            marketplace: market.to_owned(),
            digest: plugin_tree_digest(&path).ok(),
            path,
            enabled: current.is_none_or(|current| current.enabled),
            description: description.to_owned(),
            held: None,
        };
        plugins.insert(name.to_owned(), plugin.clone());
        Ok(plugin)
    });
    if result.is_err() {
        staged.discard();
    }
    result
}

/// The marketplace entry `spec` names: `name@marketplace`, or `name` when
/// only one marketplace is registered.
fn marketplace_entry(
    data: &Path,
    spec: &str,
) -> Result<(String, String, PathBuf, MarketplacePlugin)> {
    let marketplaces = marketplaces(data);
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
        .with_context(|| format!("unknown marketplace {market}; add it first"))?
        .clone();
    let entry = manifest_of(&root)?
        .plugins
        .into_iter()
        .find(|plugin| plugin.name == name)
        .with_context(|| format!("marketplace {market} has no plugin {name}"))?;
    Ok((name, market, root, entry))
}

/// Refuses to replace a plugin whose installed files were edited, as
/// marketplace installs do.
fn refuse_local_edits(data: &Path, name: &str) -> Result<()> {
    if let Some(record) = installed(data).get(name)
        && locally_modified(record)?
    {
        bail!(
            "{name} was modified locally; preserve your edits, then remove it and install it again"
        );
    }
    Ok(())
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
        let description = manifest
            .get("description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        return change_installed(data, |plugins| {
            let plugin = InstalledPlugin {
                marketplace: "local".into(),
                path,
                enabled: plugins.get(&name).is_none_or(|current| current.enabled),
                description,
                digest: None,
                held: None,
            };
            plugins.insert(name, plugin.clone());
            Ok(plugin)
        });
    }
    // Plugin names never contain a path separator.
    let name = spec.split_once('@').map_or(spec, |(name, _)| name);
    if local_path(spec).is_some() || name.contains(['/', '\\']) {
        bail!("{spec} does not exist");
    }
    let (name, market, root, entry) = marketplace_entry(data, spec)?;
    refuse_local_edits(data, &name)?;
    let staged = stage_plugin(data, &market, &name, &root, &entry)?;
    install_staged(data, &name, &market, staged, &entry.description, false)
}

pub fn set_enabled(data: &Path, name: &str, enabled: bool) -> Result<()> {
    change_installed(data, |plugins| {
        plugins
            .get_mut(name)
            .with_context(|| format!("plugin {name} is not installed"))?
            .enabled = enabled;
        Ok(())
    })
}

/// Installs the marketplace's current version of an installed plugin, as it
/// was asked for: it may add hooks, MCP servers or programs. It stays on or
/// off as it is when the new files are written, and refuses to replace local
/// edits.
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
    let (name, market, root, entry) =
        marketplace_entry(data, &format!("{name}@{}", plugin.marketplace))?;
    refuse_local_edits(data, &name)?;
    let staged = stage_plugin(data, &market, &name, &root, &entry)?;
    install_staged(data, &name, &market, staged, &entry.description, true)
}

pub fn remove(data: &Path, name: &str) -> Result<()> {
    change_installed(data, |plugins| {
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
        Ok(())
    })
}

pub fn marketplaces(data: &Path) -> BTreeMap<String, PathBuf> {
    read_map(&marketplaces_file(data))
}

/// Registered Claude-format marketplaces and what each offers, for clients
/// (and the model) that choose a plugin to install by `name@marketplace`.
pub fn marketplace_catalog(data: &Path) -> serde_json::Value {
    let installed = installed(data);
    let sources = marketplace_sources(data);
    serde_json::Value::Array(
        marketplaces(data)
            .into_iter()
            .map(|(name, root)| {
                // Where it comes from, with any credential in the address hidden.
                let origin = if !is_owned_clone(data, &name, &root) {
                    serde_json::json!({"type": "folder", "source": root, "autoUpdate": false, "commit": null, "updatedAt": null})
                } else if let Some(source) = sources.get(&name) {
                    serde_json::json!({
                        "type": "git",
                        "source": haarness_core::redact::url(&source.source),
                        "autoUpdate": source.auto_update,
                        "commit": source.commit,
                        "updatedAt": source.updated_at,
                    })
                } else {
                    // Registered before sources were recorded: it stays as it
                    // is until someone updates it.
                    serde_json::json!({
                        "type": "git",
                        "source": origin_url(&root).map(|url| haarness_core::redact::url(&url).into_owned()),
                        "autoUpdate": false,
                        "commit": null,
                        "updatedAt": null,
                    })
                };
                let mut row = match manifest_of(&root) {
                    Ok(manifest) => serde_json::json!({
                        "name": name,
                        "path": root,
                        "plugins": manifest.plugins.iter().map(|plugin| serde_json::json!({
                            "name": plugin.name,
                            "install": format!("{}@{name}", plugin.name),
                            "description": plugin.description,
                            "installed": installed.get(&plugin.name).is_some_and(|entry| entry.marketplace == name),
                        })).collect::<Vec<_>>(),
                    }),
                    Err(error) => serde_json::json!({"name": name, "path": root, "error": error.to_string()}),
                };
                if let (Some(row), Some(origin)) = (row.as_object_mut(), origin.as_object()) {
                    row.extend(origin.clone());
                }
                // Installed plugins of it whose automatic update waits, and why.
                row["held"] = serde_json::Value::Array(
                    installed
                        .iter()
                        .filter(|(_, plugin)| plugin.marketplace == name)
                        .filter_map(|(plugin, record)| {
                            record.held.as_ref().map(|reason| {
                                serde_json::json!({"plugin": plugin, "reason": reason})
                            })
                        })
                        .collect(),
                );
                row
            })
            .collect(),
    )
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
    fn a_marketplace_or_plugin_name_is_one_plain_folder_name() {
        for name in ["C:", "C:x", "a:b", "..", ".", "a/b", "a\\b", "", "a\nb"] {
            assert!(safe_name(name, "plugin").is_err(), "{name:?}");
        }
        for name in ["demo", "demo-plugin_2", "with.dot", "Ünïcode"] {
            assert!(safe_name(name, "plugin").is_ok(), "{name:?}");
        }
        // A marketplace that names itself after a drive is not added, and a
        // plugin named like one is not installed.
        let data = std::env::temp_dir().join(format!("haarness-plugins-{}", uuid::Uuid::now_v7()));
        let market = std::env::temp_dir().join(format!("haarness-market-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(market.join(".claude-plugin")).unwrap();
        fs::write(
            market.join(".claude-plugin/marketplace.json"),
            r#"{"name": "C:", "plugins": []}"#,
        )
        .unwrap();
        let error = marketplace_add(&data, market.to_str().unwrap()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("invalid marketplace name \"C:\""),
            "{error}"
        );
        assert!(marketplaces(&data).is_empty());
        fixture_marketplace(&market);
        marketplace_add(&data, market.to_str().unwrap()).unwrap();
        let error = install(&data, "C:demo@fixture").unwrap_err();
        assert!(error.to_string().contains("invalid plugin name"), "{error}");
        assert!(!plugins_root(&data).join("cache").exists());
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
                .contains("a link in the plugin leads outside its folder")
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
    fn a_failed_clone_never_quotes_the_credential_in_its_source() {
        let root = std::env::temp_dir().join(format!("haarness-clone-{}", uuid::Uuid::now_v7()));
        let data = root.join("data");
        // Nothing listens on port 1, so git fails at once without a network.
        for source in [
            "https://fixture:PRIVATE_SUFFIX@127.0.0.1:1/market.git",
            "https://127.0.0.1:1/market.git?access_token=PRIVATE_SUFFIX",
        ] {
            let error = format!("{:#}", marketplace_add(&data, source).unwrap_err());
            assert!(!error.contains("PRIVATE_SUFFIX"), "{error}");
            assert!(error.contains("git clone failed for https://"), "{error}");
            assert!(error.contains("127.0.0.1:1/market.git"), "{error}");
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn git_sources_check_out_their_pinned_sha_or_ref_and_url_paths() {
        let root = std::env::temp_dir().join(format!("haarness-pins-{}", uuid::Uuid::now_v7()));
        let data = root.join("data");
        let repo = root.join("repo");
        let git = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args([
                        "-c",
                        "user.name=Haarness Test",
                        "-c",
                        "user.email=test@example.invalid"
                    ])
                    .args(args)
                    .current_dir(&repo)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        let mut commits = Vec::new();
        for version in ["one", "two"] {
            for dir in [repo.clone(), repo.join("plugins/kit")] {
                fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
                fs::write(
                    dir.join(".claude-plugin/plugin.json"),
                    format!(r#"{{"name":"kit","version":"{version}"}}"#),
                )
                .unwrap();
            }
            if version == "one" {
                git(&["init", "-q", "-b", "main"]);
            }
            git(&["add", "."]);
            git(&["commit", "-qm", version]);
            if version == "one" {
                git(&["tag", "v1"]);
            }
            let head = Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&repo)
                .output()
                .unwrap();
            commits.push(String::from_utf8(head.stdout).unwrap().trim().to_owned());
        }
        let url = format!("file://{}", repo.display());
        let market = root.join("market");
        fs::create_dir_all(market.join(".claude-plugin")).unwrap();
        fs::write(
            market.join(".claude-plugin/marketplace.json"),
            serde_json::json!({"name": "pins", "plugins": [
                {"name": "by-sha", "source": {"source": "url", "url": url, "ref": "main", "sha": commits[0]}},
                {"name": "by-ref", "source": {"source": "url", "url": url, "ref": "v1"}},
                {"name": "subdir-sha", "source": {"source": "git-subdir", "url": url, "path": "plugins/kit", "sha": commits[0]}},
                {"name": "url-path", "source": {"source": "url", "url": url, "path": "plugins/kit"}},
                {"name": "latest", "source": {"source": "url", "url": url}},
                {"name": "short-sha", "source": {"source": "url", "url": url, "sha": &commits[0][..7]}},
                {"name": "option", "source": {"source": "url", "url": "--upload-pack=touch injected"}}
            ]})
            .to_string(),
        )
        .unwrap();
        marketplace_add(&data, market.to_str().unwrap()).unwrap();
        let version = |plugin: &InstalledPlugin| {
            let manifest: serde_json::Value = serde_json::from_slice(
                &fs::read(plugin.path.join(".claude-plugin/plugin.json")).unwrap(),
            )
            .unwrap();
            manifest["version"].as_str().unwrap().to_owned()
        };
        for (name, expected) in [
            ("by-sha", "one"),
            ("by-ref", "one"),
            ("subdir-sha", "one"),
            ("latest", "two"),
        ] {
            assert_eq!(
                version(&install(&data, &format!("{name}@pins")).unwrap()),
                expected,
                "{name}"
            );
        }
        let subdir = install(&data, "url-path@pins").unwrap();
        assert!(
            subdir.path.ends_with("plugins/kit"),
            "{}",
            subdir.path.display()
        );
        // Updating re-fetches the same pinned commit.
        assert_eq!(version(&update(&data, "by-sha").unwrap()), "one");
        assert!(
            install(&data, "short-sha@pins")
                .unwrap_err()
                .to_string()
                .contains("40-character")
        );
        assert!(
            install(&data, "option@pins")
                .unwrap_err()
                .to_string()
                .contains("invalid git URL")
        );
        assert!(!repo.join("injected").exists() && !Path::new("injected").exists());
        fs::remove_dir_all(root).unwrap();
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

    /// A marketplace repository served from a bare repository, as a Git host
    /// would: `commit` writes files into the working copy and pushes them.
    struct Upstream {
        work: PathBuf,
        url: String,
    }

    impl Upstream {
        fn new(root: &Path) -> Self {
            let bare = root.join("upstream.git");
            let work = root.join("upstream");
            fs::create_dir_all(&work).unwrap();
            let run = |dir: &Path, args: &[&str]| {
                assert!(
                    Command::new("git")
                        .args(["-c", "init.defaultBranch=main"])
                        .args(args)
                        .current_dir(dir)
                        .status()
                        .unwrap()
                        .success(),
                    "{args:?}"
                );
            };
            run(root, &["init", "-q", "--bare", bare.to_str().unwrap()]);
            run(&work, &["init", "-q"]);
            run(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
            Self {
                work,
                url: format!("file://{}", bare.display()),
            }
        }

        fn commit(&self, files: &[(&str, &str)], message: &str) -> String {
            for (path, body) in files {
                let path = self.work.join(path);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, body).unwrap();
            }
            for args in [
                vec!["add", "-A"],
                vec![
                    "-c",
                    "user.name=Haarness Test",
                    "-c",
                    "user.email=test@example.invalid",
                    "commit",
                    "-qm",
                    message,
                ],
                vec!["push", "-q", "origin", "HEAD:main"],
            ] {
                assert!(
                    Command::new("git")
                        .args(&args)
                        .current_dir(&self.work)
                        .status()
                        .unwrap()
                        .success(),
                    "{args:?}"
                );
            }
            head_commit(&self.work).unwrap()
        }
    }

    fn market_manifest(name: &str, plugins: &[&str]) -> String {
        serde_json::json!({
            "name": name,
            "plugins": plugins.iter().map(|plugin| serde_json::json!({
                "name": plugin, "source": format!("./plugins/{plugin}"), "description": plugin
            })).collect::<Vec<_>>(),
        })
        .to_string()
    }

    fn plugin_files(name: &str, command: &str) -> Vec<(String, String)> {
        vec![
            (
                format!("plugins/{name}/.claude-plugin/plugin.json"),
                format!(r#"{{"name":"{name}"}}"#),
            ),
            (
                format!("plugins/{name}/commands/run.md"),
                command.to_owned(),
            ),
        ]
    }

    fn files(groups: &[Vec<(String, String)>]) -> Vec<(&str, &str)> {
        groups
            .iter()
            .flatten()
            .map(|(path, body)| (path.as_str(), body.as_str()))
            .collect()
    }

    #[test]
    fn a_git_marketplace_follows_new_commits_and_updates_the_plugins_that_changed() {
        let root = std::env::temp_dir().join(format!("haarness-refresh-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        let data = root.join("data");
        let upstream = Upstream::new(&root);
        let manifest = market_manifest("team", &["kit", "steady"]);
        let first = upstream.commit(
            &[
                files(&[
                    plugin_files("kit", "Version one"),
                    plugin_files("steady", "Steady"),
                ]),
                vec![(".claude-plugin/marketplace.json", manifest.as_str())],
            ]
            .concat(),
            "one",
        );
        assert_eq!(marketplace_add(&data, &upstream.url).unwrap(), "team");
        // A new Git registration follows its repository from the start.
        let source = &marketplace_sources(&data)["team"];
        assert_eq!(source.source, upstream.url);
        assert!(source.auto_update);
        assert_eq!(source.commit.as_deref(), Some(first.as_str()));
        assert!(source.updated_at.is_some());
        // marketplaces.json keeps the shape older versions read: name → path.
        let registered: BTreeMap<String, PathBuf> =
            serde_json::from_str(&fs::read_to_string(marketplaces_file(&data)).unwrap()).unwrap();
        assert_eq!(registered["team"], owned_clone(&data, "team"));
        let kit = install(&data, "kit@team").unwrap();
        install(&data, "steady@team").unwrap();
        set_enabled(&data, "kit", false).unwrap();
        let command = kit.path.join("commands/run.md");
        let steady = installed(&data)["steady"].path.join("commands/run.md");
        let steady_written = fs::metadata(&steady).unwrap().modified().unwrap();

        // Nothing new upstream: nothing is fetched or rewritten.
        let refresh = marketplace_update(&data, "team").unwrap();
        assert_eq!(refresh.status, "unchanged");
        assert_eq!(refresh.commit.as_deref(), Some(first.as_str()));
        assert!(!refresh.changed());

        // A new commit changes one plugin: that one is updated, still off.
        let second = upstream.commit(&files(&[plugin_files("kit", "Version two")]), "two");
        let refresh = marketplace_update(&data, "team").unwrap();
        assert_eq!(refresh.status, "updated");
        assert_eq!(refresh.previous.as_deref(), Some(first.as_str()));
        assert_eq!(refresh.commit.as_deref(), Some(second.as_str()));
        assert_eq!(refresh.plugins, ["kit"]);
        assert!(refresh.failed.is_empty() && refresh.unlisted.is_empty());
        assert_eq!(fs::read_to_string(&command).unwrap(), "Version two");
        assert!(!installed(&data)["kit"].enabled);
        assert_eq!(fs::read_to_string(&steady).unwrap(), "Steady");
        assert_eq!(
            fs::metadata(&steady).unwrap().modified().unwrap(),
            steady_written
        );
        assert_eq!(
            marketplace_sources(&data)["team"].commit.as_deref(),
            Some(second.as_str())
        );
        assert_eq!(head_commit(&owned_clone(&data, "team")).unwrap(), second);
        // No staging or previous clone is left beside it.
        let leftovers = fs::read_dir(plugins_root(&data).join("marketplaces"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(leftovers, ["team"]);

        // A plugin the marketplace stops listing stays installed as it is.
        let third = upstream.commit(
            &[(
                ".claude-plugin/marketplace.json",
                market_manifest("team", &["kit"]).as_str(),
            )],
            "three",
        );
        let refresh = marketplace_update(&data, "team").unwrap();
        assert_eq!(refresh.unlisted, ["steady"]);
        assert!(refresh.plugins.is_empty());
        assert_eq!(fs::read_to_string(&steady).unwrap(), "Steady");

        // A marketplace that renames itself is refused, and a broken one
        // too; either way the clone that worked stays.
        upstream.commit(
            &[(
                ".claude-plugin/marketplace.json",
                market_manifest("renamed", &["kit"]).as_str(),
            )],
            "rename",
        );
        let error = marketplace_update(&data, "team").unwrap_err().to_string();
        assert!(error.contains("now calls itself \"renamed\""), "{error}");
        upstream.commit(
            &[
                (".claude-plugin/marketplace.json", "{ not json"),
                ("plugins/kit/commands/run.md", "Version broken"),
            ],
            "broken",
        );
        assert!(marketplace_update(&data, "team").is_err());
        assert_eq!(head_commit(&owned_clone(&data, "team")).unwrap(), third);
        assert_eq!(
            marketplace_sources(&data)["team"].commit.as_deref(),
            Some(third.as_str())
        );
        assert_eq!(
            manifest_of(&owned_clone(&data, "team")).unwrap().name,
            "team"
        );
        assert_eq!(fs::read_to_string(&command).unwrap(), "Version two");
        assert_eq!(
            fs::read_dir(plugins_root(&data).join("marketplaces"))
                .unwrap()
                .count(),
            1
        );
        // Fixed upstream, it updates again.
        upstream.commit(
            &[
                (
                    ".claude-plugin/marketplace.json",
                    market_manifest("team", &["kit"]).as_str(),
                ),
                ("plugins/kit/commands/run.md", "Version three"),
            ],
            "fixed",
        );
        let refresh = marketplace_update(&data, "team").unwrap();
        assert_eq!(refresh.plugins, ["kit"]);
        assert_eq!(fs::read_to_string(&command).unwrap(), "Version three");

        // Turned off, the background refresh leaves it; `update` still works.
        marketplace_auto_update(&data, "team", false).unwrap();
        assert!(!marketplace_sources(&data)["team"].auto_update);
        upstream.commit(&[("plugins/kit/commands/run.md", "Version four")], "four");
        assert!(marketplace_update_all(&data, true).is_empty());
        assert_eq!(fs::read_to_string(&command).unwrap(), "Version three");
        marketplace_auto_update(&data, "team", true).unwrap();
        let refreshed = marketplace_update_all(&data, true);
        assert_eq!(refreshed.len(), 1);
        assert_eq!(refreshed[0].plugins, ["kit"]);
        assert_eq!(fs::read_to_string(&command).unwrap(), "Version four");
        let listed = marketplace_catalog(&data);
        assert_eq!(listed[0]["type"], "git");
        assert_eq!(listed[0]["source"], upstream.url);
        assert_eq!(listed[0]["autoUpdate"], true);
        assert!(listed[0]["commit"].is_string() && listed[0]["updatedAt"].is_string());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn marketplaces_registered_before_sources_were_recorded_keep_working() {
        let root = std::env::temp_dir().join(format!("haarness-legacy-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        let data = root.join("data");
        let upstream = Upstream::new(&root);
        let manifest = market_manifest("team", &["kit"]);
        let first = upstream.commit(
            &[
                files(&[plugin_files("kit", "Version one")]),
                vec![(".claude-plugin/marketplace.json", manifest.as_str())],
            ]
            .concat(),
            "one",
        );
        // As an older Haarness left it: a clone and marketplaces.json, no sidecar.
        let clone = owned_clone(&data, "team");
        git_clone(&upstream.url, &clone).unwrap();
        let folder = root.join("folder");
        fixture_marketplace(&folder);
        write_map(
            &marketplaces_file(&data),
            &BTreeMap::from([
                ("team".to_owned(), clone.clone()),
                ("fixture".to_owned(), folder.canonicalize().unwrap()),
            ]),
        )
        .unwrap();
        assert!(!sources_file(&data).exists());
        let kit = install(&data, "kit@team").unwrap();
        install(&data, "demo@fixture").unwrap();
        let listed = marketplace_catalog(&data);
        let row = |name: &str| {
            listed
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["name"] == name)
                .unwrap()
                .clone()
        };
        assert_eq!(row("team")["type"], "git");
        assert_eq!(row("team")["source"], upstream.url);
        assert_eq!(row("team")["autoUpdate"], false);
        assert_eq!(row("fixture")["type"], "folder");
        assert_eq!(row("fixture")["autoUpdate"], false);
        // No background refresh for either.
        assert!(marketplace_update_all(&data, true).is_empty());
        // Updating it by hand records where it came from, without following it.
        let refresh = marketplace_update(&data, "team").unwrap();
        assert_eq!(refresh.status, "unchanged");
        let source = &marketplace_sources(&data)["team"];
        assert_eq!(source.source, upstream.url);
        assert!(!source.auto_update);
        assert_eq!(source.commit.as_deref(), Some(first.as_str()));
        upstream.commit(&[("plugins/kit/commands/run.md", "Version two")], "two");
        assert_eq!(marketplace_update(&data, "team").unwrap().plugins, ["kit"]);
        assert_eq!(
            fs::read_to_string(kit.path.join("commands/run.md")).unwrap(),
            "Version two"
        );
        assert!(!marketplace_sources(&data)["team"].auto_update);
        // A folder is read as it is: nothing to fetch, nothing to follow.
        assert_eq!(
            marketplace_update(&data, "fixture").unwrap().status,
            "folder"
        );
        assert!(
            marketplace_auto_update(&data, "fixture", true)
                .unwrap_err()
                .to_string()
                .contains("folder on this computer")
        );
        assert!(!marketplace_sources(&data).contains_key("fixture"));
        // Registering a folder under a Git marketplace's name drops its source.
        marketplace_auto_update(&data, "team", true).unwrap();
        let replacement = root.join("replacement");
        fs::create_dir_all(replacement.join(".claude-plugin")).unwrap();
        fs::write(
            replacement.join(".claude-plugin/marketplace.json"),
            market_manifest("team", &[]),
        )
        .unwrap();
        marketplace_add(&data, replacement.to_str().unwrap()).unwrap();
        assert!(!marketplace_sources(&data).contains_key("team"));
        assert!(
            marketplace_update(&data, "missing")
                .unwrap_err()
                .to_string()
                .contains("unknown marketplace missing")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn plugin_actions_refresh_and_follow_marketplaces() {
        let root = std::env::temp_dir().join(format!("haarness-actions-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        let data = root.join("data");
        let upstream = Upstream::new(&root);
        let manifest = market_manifest("team", &["kit"]);
        upstream.commit(
            &[
                files(&[plugin_files("kit", "Version one")]),
                vec![(".claude-plugin/marketplace.json", manifest.as_str())],
            ]
            .concat(),
            "one",
        );
        let service = PluginService::new(
            data.clone(),
            root.join("config"),
            root.join("workspace"),
            false,
        );
        let apply = |action: &str, target: &str| {
            haarness_server::PluginRegistry::apply(
                &service,
                &PluginActionRequest {
                    action: action.into(),
                    target: target.into(),
                },
            )
        };
        apply("marketplace", &upstream.url).unwrap();
        apply("install", "kit@team").unwrap();
        apply("marketplace_auto_update", "team:off").unwrap();
        assert!(!marketplace_sources(&data)["team"].auto_update);
        apply("marketplace_auto_update", "team").unwrap();
        assert!(marketplace_sources(&data)["team"].auto_update);
        assert!(apply("marketplace_auto_update", "team:maybe").is_err());
        upstream.commit(&[("plugins/kit/commands/run.md", "Version two")], "two");
        apply("marketplace_update", "").unwrap();
        assert_eq!(
            fs::read_to_string(installed(&data)["kit"].path.join("commands/run.md")).unwrap(),
            "Version two"
        );
        assert!(apply("marketplace_update", "missing").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    /// A marketplace with one plugin at `upstream`, registered and installed.
    fn installed_kit(root: &Path) -> (PathBuf, Upstream, PathBuf) {
        let data = root.join("data");
        let upstream = Upstream::new(root);
        let manifest = market_manifest("team", &["kit"]);
        upstream.commit(
            &[
                files(&[plugin_files("kit", "Version one")]),
                vec![(".claude-plugin/marketplace.json", manifest.as_str())],
            ]
            .concat(),
            "one",
        );
        marketplace_add(&data, &upstream.url).unwrap();
        let command = install(&data, "kit@team")
            .unwrap()
            .path
            .join("commands/run.md");
        (data, upstream, command)
    }

    #[test]
    fn a_plugin_switched_off_or_removed_while_its_update_is_fetched_stays_so() {
        let root = std::env::temp_dir().join(format!("haarness-race-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        let (data, upstream, command) = installed_kit(&root);
        upstream.commit(&[("plugins/kit/commands/run.md", "Version two")], "two");
        marketplace_update(&data, "team").unwrap();
        let (_, market, clone, entry) = marketplace_entry(&data, "kit@team").unwrap();
        // Fetched, then switched off before the new files are recorded.
        let staged = stage_plugin(&data, &market, "kit", &clone, &entry).unwrap();
        set_enabled(&data, "kit", false).unwrap();
        install_staged(&data, "kit", &market, staged, "", true).unwrap();
        assert!(!installed(&data)["kit"].enabled);
        // Fetched, then removed: it stays removed, and nothing is left behind.
        let staged = stage_plugin(&data, &market, "kit", &clone, &entry).unwrap();
        let staging = staged.root.clone();
        remove(&data, "kit").unwrap();
        let error = install_staged(&data, "kit", &market, staged, "", true).unwrap_err();
        assert!(error.to_string().contains("stays removed"), "{error}");
        assert!(!installed(&data).contains_key("kit"));
        assert!(!staging.exists() && !command.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_automatic_update_keeps_local_edits_and_waits_for_plugins_that_ask_for_more() {
        let root = std::env::temp_dir().join(format!("haarness-held-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        let (data, upstream, command) = installed_kit(&root);
        assert!(installed(&data)["kit"].digest.is_some());
        // Local edits are never replaced, by the refresh or by hand.
        fs::write(&command, "My own version").unwrap();
        upstream.commit(&[("plugins/kit/commands/run.md", "Version two")], "two");
        let refresh = marketplace_update(&data, "team").unwrap();
        assert!(refresh.plugins.is_empty());
        assert_eq!(refresh.held[0].plugin, "kit");
        assert_eq!(refresh.held[0].reason, "has local edits");
        assert_eq!(fs::read_to_string(&command).unwrap(), "My own version");
        assert_eq!(
            installed(&data)["kit"].held.as_deref(),
            Some("has local edits")
        );
        assert_eq!(
            marketplace_catalog(&data)[0]["held"],
            serde_json::json!([{"plugin": "kit", "reason": "has local edits"}])
        );
        assert!(
            update(&data, "kit")
                .unwrap_err()
                .to_string()
                .contains("modified locally")
        );
        assert_eq!(fs::read_to_string(&command).unwrap(), "My own version");
        // Edits undone, the next refresh updates it and the hold is gone.
        fs::write(&command, "Version one").unwrap();
        upstream.commit(&[("plugins/kit/commands/other.md", "More")], "three");
        let refresh = marketplace_update(&data, "team").unwrap();
        assert_eq!(refresh.plugins, ["kit"]);
        assert_eq!(fs::read_to_string(&command).unwrap(), "Version two");
        assert!(installed(&data)["kit"].held.is_none());

        // A version that adds hooks, an MCP server or a program waits for
        // the person; `plugin update` installs it.
        for (path, body, reason) in [
            (
                "plugins/kit/hooks/hooks.json",
                r#"{"hooks":{"SessionStart":[]}}"#,
                "asks for more: new hooks",
            ),
            (
                "plugins/kit/.mcp.json",
                r#"{"mcpServers":{"docs":{"command":"docs"}}}"#,
                "asks for more: new MCP servers",
            ),
        ] {
            upstream.commit(&[(path, body)], reason);
            let refresh = marketplace_update(&data, "team").unwrap();
            assert!(refresh.plugins.is_empty(), "{reason}");
            assert_eq!(refresh.held[0].reason, reason);
            let installed_file = installed(&data)["kit"]
                .path
                .join(path.trim_start_matches("plugins/kit/"));
            assert!(!installed_file.exists(), "{reason}");
            assert_eq!(installed(&data)["kit"].held.as_deref(), Some(reason));
            update(&data, "kit").unwrap();
            assert!(installed_file.exists(), "{reason}");
            assert!(installed(&data)["kit"].held.is_none());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let script = upstream.work.join("plugins/kit/bin/run.sh");
            fs::create_dir_all(script.parent().unwrap()).unwrap();
            fs::write(&script, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
            upstream.commit(&[], "program");
            let refresh = marketplace_update(&data, "team").unwrap();
            assert_eq!(refresh.held[0].reason, "asks for more: new programs");
            // An ordinary file of a plugin that already has its hooks is no reason.
            update(&data, "kit").unwrap();
            upstream.commit(&[("plugins/kit/commands/run.md", "Version three")], "text");
            let refresh = marketplace_update(&data, "team").unwrap();
            assert_eq!(refresh.plugins, ["kit"]);
            assert!(refresh.held.is_empty());
        }
        // Changed hooks wait too.
        upstream.commit(
            &[("plugins/kit/hooks/hooks.json", r#"{"hooks":{"Stop":[]}}"#)],
            "changed hooks",
        );
        assert_eq!(
            marketplace_update(&data, "team").unwrap().held[0].reason,
            "asks for more: changed hooks"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_clone_lost_between_the_two_renames_of_a_swap_comes_back() {
        let root = std::env::temp_dir().join(format!("haarness-lost-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        let (data, upstream, command) = installed_kit(&root);
        let clone = owned_clone(&data, "team");
        let head = head_commit(&clone).unwrap();
        // Stopped after moving the clone aside: it is put back.
        let aside = plugins_root(&data)
            .join("marketplaces")
            .join(format!(".team.previous-{}", uuid::Uuid::now_v7()));
        fs::rename(&clone, &aside).unwrap();
        assert_eq!(
            marketplace_update(&data, "team").unwrap().status,
            "unchanged"
        );
        assert_eq!(head_commit(&clone).unwrap(), head);
        assert!(!aside.exists());
        // Gone with nothing to put back: it is cloned again.
        fs::remove_dir_all(&clone).unwrap();
        upstream.commit(&[("plugins/kit/commands/run.md", "Version two")], "two");
        let refresh = marketplace_update(&data, "team").unwrap();
        assert_eq!(refresh.status, "updated");
        assert_eq!(refresh.plugins, ["kit"]);
        assert_eq!(fs::read_to_string(&command).unwrap(), "Version two");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_ref_means_that_branch_or_tag_and_nothing_ending_in_the_same_name() {
        let root = std::env::temp_dir().join(format!("haarness-refs-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        let upstream = Upstream::new(&root);
        let tagged = upstream.commit(&[("file", "one")], "one");
        let run = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(["-c", "user.name=T", "-c", "user.email=t@example.invalid"])
                    .args(args)
                    .current_dir(&upstream.work)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        run(&["tag", "-a", "v1", "-m", "release"]);
        run(&["push", "-q", "origin", "v1"]);
        // A branch whose name ends in the tag's, on another commit.
        run(&["checkout", "-q", "-b", "feature/v1"]);
        fs::write(upstream.work.join("file"), "two").unwrap();
        run(&["commit", "-qam", "two"]);
        run(&["push", "-q", "origin", "feature/v1"]);
        let feature = head_commit(&upstream.work).unwrap();
        run(&["checkout", "-q", "main"]);
        assert_eq!(remote_commit(&upstream.url, Some("v1")).unwrap(), tagged);
        assert_eq!(
            remote_commit(&upstream.url, Some("feature/v1")).unwrap(),
            feature
        );
        assert_eq!(remote_commit(&upstream.url, Some("main")).unwrap(), tagged);
        assert_eq!(remote_commit(&upstream.url, None).unwrap(), tagged);
        assert!(remote_commit(&upstream.url, Some("1")).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_never_waits_on_a_terminal_or_a_silent_host() {
        // No input: a command that reads it ends at once.
        let args = ["-c", "alias.reads=!cat", "reads"]
            .map(std::ffi::OsString::from)
            .to_vec();
        assert_eq!(
            git_output_within(&args, Duration::from_secs(20)).unwrap(),
            ""
        );
        // A host that accepts and never answers is given up on.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/market.git", listener.local_addr().unwrap());
        let started = std::time::Instant::now();
        let args = ["ls-remote", "--", &url, "HEAD"]
            .map(std::ffi::OsString::from)
            .to_vec();
        let error = git_output_within(&args, Duration::from_secs(2)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("did not finish within 2 seconds"),
            "{error}"
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        drop(listener);
    }
}
