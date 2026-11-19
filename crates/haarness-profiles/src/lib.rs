//! Portable configuration archives and an immutable, authenticated-server catalog.
//! Archives are decoded in memory, never extracted using archive-provided paths.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::Uuid;
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

#[cfg(test)]
mod tests;

pub const MAX_ARCHIVE: usize = 32 * 1024 * 1024;
const MAX_EXPANDED: usize = 64 * 1024 * 1024;
const MAX_MANIFEST: u64 = 2 * 1024 * 1024;
const CONFIG_FILES: &[&str] = &[
    "config.toml",
    "haarness.json",
    "haarness.jsonc",
    "opencode.json",
    "opencode.jsonc",
    "AGENTS.md",
    "settings.json",
    "settings.local.json",
    "tui.json",
    "tui.jsonc",
    "extensions.json",
];
const CONFIG_DIRS: &[&str] = &[
    "agents",
    "agent",
    "commands",
    "command",
    "skills",
    "hooks",
    "workflows",
    "output-styles",
    "themes",
    "extensions",
];
const PROJECT_DIRS: &[&str] = &[".haarness", ".agents", ".opencode"];

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Global,
    Local,
    #[default]
    Both,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Entry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub executable: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub format: String,
    pub version: u32,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub scope: Scope,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub id: Uuid,
    pub name: String,
    pub scope: Scope,
    pub created_at: DateTime<Utc>,
    pub uploaded_at: DateTime<Utc>,
    pub entry_count: usize,
    pub size_bytes: usize,
    pub sha256: String,
}

#[derive(Clone, Debug)]
pub struct Roots {
    pub config: PathBuf,
    pub data: PathBuf,
    pub project: PathBuf,
}

pub struct Archive {
    pub manifest: Manifest,
    files: BTreeMap<String, Vec<u8>>,
}

/// A saved profile archive, and the symbolic links it was saved without
/// because they lead to files a profile does not save.
pub struct Saved {
    pub bytes: Vec<u8>,
    pub left_out: Vec<PathBuf>,
}

/// The files a restore writes (or, in a dry run, would write), and those it
/// leaves alone because they are already in place: a link on the way to
/// them stays inside the same folder and the file it leads to already holds
/// the profile's bytes, as when a profile is restored into the project it
/// was saved from.
#[derive(Debug)]
pub struct Restored {
    pub targets: Vec<PathBuf>,
    pub in_place: Vec<PathBuf>,
}

#[derive(Deserialize)]
struct Installed {
    path: PathBuf,
    enabled: bool,
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 1024
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with(['.', ' '])
                && !part
                    .chars()
                    .any(|c| c.is_control() || "\\:*?\"<>|".contains(c))
                && !matches!(
                    part.split('.')
                        .next()
                        .unwrap_or_default()
                        .to_ascii_uppercase()
                        .as_str(),
                    "CON"
                        | "PRN"
                        | "AUX"
                        | "NUL"
                        | "COM1"
                        | "COM2"
                        | "COM3"
                        | "COM4"
                        | "COM5"
                        | "COM6"
                        | "COM7"
                        | "COM8"
                        | "COM9"
                        | "LPT1"
                        | "LPT2"
                        | "LPT3"
                        | "LPT4"
                        | "LPT5"
                        | "LPT6"
                        | "LPT7"
                        | "LPT8"
                        | "LPT9"
                )
        })
}

fn excluded(path: &str) -> bool {
    path.split('/').any(|part| {
        let part = part.to_ascii_lowercase();
        matches!(
            part.as_str(),
            ".git"
                | "node_modules"
                | "target"
                | "auth.json"
                | "mcp-auth.json"
                | "mcp-oauth"
                | "credentials.json"
        ) || part == ".env"
            || part.starts_with(".env.")
            || Path::new(&part).extension().is_some_and(|extension| {
                extension.eq_ignore_ascii_case("pem") || extension.eq_ignore_ascii_case("key")
            })
    })
}

/// What macOS leaves beside configuration: Finder's `.DS_Store`, the
/// AppleDouble `._name` files written on volumes without extended attributes,
/// and the `__MACOSX` folder of an unpacked Finder zip. An export leaves them
/// out; archives that already carry them still restore.
fn macos_clutter(name: &str) -> bool {
    name == ".DS_Store" || name.starts_with("._") || name == "__MACOSX"
}

fn config_path(path: &str) -> bool {
    CONFIG_FILES.contains(&path)
        || path.starts_with("plugins/local/")
        || path
            .split_once('/')
            .is_some_and(|(first, _)| CONFIG_DIRS.contains(&first))
}

fn allowed(path: &str, scope: Scope) -> bool {
    if !safe_path(path) || excluded(path) {
        return false;
    }
    if let Some(path) = path.strip_prefix("global/config/") {
        return scope != Scope::Local && config_path(path);
    }
    if let Some(path) = path.strip_prefix("global/data/") {
        return scope != Scope::Local
            && (path.starts_with("skills/") || path.starts_with("plugins/local/"));
    }
    if let Some(path) = path.strip_prefix("project/") {
        if scope == Scope::Global {
            return false;
        }
        return CONFIG_FILES.contains(&path)
            || path.split_once('/').is_some_and(|(first, rest)| {
                PROJECT_DIRS.contains(&first)
                    && (config_path(rest) || (first == ".haarness" && rest.starts_with("plugins/")))
            });
    }
    false
}

/// Reject links in any existing component, including links above a configured root.
pub fn check_path(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => ensure!(
                !metadata.file_type().is_symlink(),
                "symlink path refused: {}",
                ancestor.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub fn read(path: &Path) -> Result<Vec<u8>> {
    let file = File::open(path).context("open profile archive")?;
    ensure!(
        file.metadata()?.is_file(),
        "profile archive must be a regular file"
    );
    let mut bytes = Vec::new();
    file.take(MAX_ARCHIVE as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_ARCHIVE, "profile archive exceeds 32 MiB");
    Ok(bytes)
}

impl Archive {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() <= MAX_ARCHIVE, "profile archive exceeds 32 MiB");
        let mut zip = ZipArchive::new(Cursor::new(bytes)).context("invalid profile ZIP")?;
        let manifest: Manifest = {
            let mut file = zip
                .by_name("manifest.json")
                .context("profile manifest missing")?;
            ensure!(file.size() <= MAX_MANIFEST, "profile manifest too large");
            let mut data = Vec::new();
            (&mut file).take(MAX_MANIFEST + 1).read_to_end(&mut data)?;
            ensure!(
                data.len() as u64 <= MAX_MANIFEST,
                "profile manifest too large"
            );
            serde_json::from_slice(&data).context("invalid profile manifest")?
        };
        ensure!(
            manifest.format == "haarness.profile" && manifest.version == 1,
            "unsupported profile format/version"
        );
        validate_name(&manifest.name)?;
        let mut expected = BTreeMap::new();
        let mut folded = BTreeSet::new();
        for entry in &manifest.entries {
            ensure!(
                allowed(&entry.path, manifest.scope),
                "profile path is not portable configuration: {}",
                entry.path
            );
            ensure!(
                folded.insert(entry.path.to_lowercase()),
                "duplicate/case-colliding profile path"
            );
            ensure!(
                expected.insert(entry.path.clone(), entry).is_none(),
                "duplicate manifest entry"
            );
        }
        // A file cannot also be the parent directory of another file.
        for path in expected.keys() {
            let mut parent = path.as_str();
            while let Some((prefix, _)) = parent.rsplit_once('/') {
                ensure!(
                    !folded.contains(&prefix.to_lowercase()),
                    "overlapping profile paths"
                );
                parent = prefix;
            }
        }
        let mut files = BTreeMap::new();
        let mut seen = BTreeSet::new();
        let mut total = 0_usize;
        for index in 0..zip.len() {
            let mut file = zip.by_index(index)?;
            let path = file.name().to_owned();
            ensure!(seen.insert(path.clone()), "duplicate ZIP entry");
            ensure!(
                !file.is_dir()
                    && file
                        .unix_mode()
                        .is_none_or(|mode| mode & 0o170_000 == 0 || mode & 0o170_000 == 0o100_000),
                "profile contains a non-regular entry"
            );
            if path == "manifest.json" {
                continue;
            }
            let entry = expected
                .get(&path)
                .context("ZIP entry is not listed in manifest")?;
            ensure!(
                file.size() == entry.size && file.size() <= MAX_EXPANDED as u64,
                "invalid profile entry size"
            );
            let mut data = Vec::new();
            (&mut file)
                .take((MAX_EXPANDED - total) as u64 + 1)
                .read_to_end(&mut data)?;
            total += data.len();
            ensure!(total <= MAX_EXPANDED, "expanded profile exceeds 64 MiB");
            ensure!(
                data.len() as u64 == entry.size && hash(&data) == entry.sha256,
                "profile checksum mismatch: {path}"
            );
            files.insert(path, data);
        }
        ensure!(
            files.len() == expected.len(),
            "profile archive is missing files"
        );
        Ok(Self { manifest, files })
    }

    pub fn capture(roots: &Roots, name: &str, scope: Scope) -> Result<Saved> {
        validate_name(name)?;
        let mut files = BTreeMap::new();
        let mut walk = Walk::default();
        if scope != Scope::Local {
            let config = Place::new(&roots.config, "global/config", Reach::Saved);
            collect(
                &roots.config,
                "global/config",
                &config,
                scope,
                &mut files,
                &mut walk,
            )?;
            // The walk above never enters `plugins/`, whose only portable part
            // is `plugins/local/` (global plugins and marketplace MCP bundles).
            collect(
                &roots.config.join("plugins/local"),
                "global/config/plugins/local",
                &config,
                scope,
                &mut files,
                &mut walk,
            )?;
            let data = Place::new(&roots.data, "global/data", Reach::Saved);
            for name in ["skills", "plugins/local"] {
                collect(
                    &roots.data.join(name),
                    &format!("global/data/{name}"),
                    &data,
                    scope,
                    &mut files,
                    &mut walk,
                )?;
            }
            // Bundle installed content instead of copying machine-specific registry paths.
            let registry = roots.data.join("plugins/installed.json");
            if registry.exists() {
                check_path(&registry)?;
                let plugins: BTreeMap<String, Installed> =
                    serde_json::from_slice(&read(&registry)?)?;
                for (name, plugin) in plugins {
                    ensure!(plugin.path.is_dir(), "installed plugin is missing: {name}");
                    let prefix = format!(
                        "global/config/plugins/local/profile-{}",
                        &hash(name.as_bytes())[..16]
                    );
                    // The registry names the plugin's folder, wherever it
                    // is, so that folder is the one its links stay inside.
                    let place = Place::new(&plugin.path, &prefix, Reach::Whole);
                    collect(&plugin.path, &prefix, &place, scope, &mut files, &mut walk)?;
                    if !plugin.enabled {
                        files.insert(format!("{prefix}/.haarness-disabled"), (Vec::new(), false));
                    }
                }
            }
        }
        if scope != Scope::Global {
            let project = Place::new(&roots.project, "project", Reach::SavedOrMarkdown);
            for name in CONFIG_FILES.iter().chain(PROJECT_DIRS.iter()) {
                collect(
                    &roots.project.join(name),
                    &format!("project/{name}"),
                    &project,
                    scope,
                    &mut files,
                    &mut walk,
                )?;
            }
        }
        // New profiles restore resources into the canonical configuration
        // plane. Migrations retain legacy files: collapse identical copies,
        // but preserve distinct legacy content without replacing newer config.
        let legacy: Vec<_> = files
            .keys()
            .filter(|path| {
                path.starts_with("global/data/skills/")
                    || path.starts_with("global/data/plugins/local/")
            })
            .cloned()
            .collect();
        for path in legacy {
            let canonical = path.replacen("global/data/", "global/config/", 1);
            if !files.contains_key(&canonical) {
                let value = files.remove(&path).expect("legacy entry exists");
                files.insert(canonical, value);
            } else if files.get(&canonical) == files.get(&path) {
                files.remove(&path);
            }
        }
        for (path, (data, _)) in &mut files {
            if is_machine_settings(path) {
                *data = with_machine_settings(data, None);
            }
        }
        let manifest = Manifest {
            format: "haarness.profile".into(),
            version: 1,
            name: name.into(),
            created_at: Utc::now(),
            scope,
            entries: files
                .iter()
                .map(|(path, (data, executable))| Entry {
                    path: path.clone(),
                    size: data.len() as u64,
                    sha256: hash(data),
                    executable: *executable,
                })
                .collect(),
        };
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o600);
        zip.start_file("manifest.json", options)?;
        zip.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
        for (path, (data, _)) in files {
            zip.start_file(path, options)?;
            zip.write_all(&data)?;
        }
        let bytes = zip.finish()?.into_inner();
        Self::decode(&bytes)?;
        let mut left_out = walk.left_out;
        left_out.sort();
        left_out.dedup();
        Ok(Saved { bytes, left_out })
    }

    /// Validate every target before making changes; stage all replacements and roll back failures.
    pub fn restore(&self, roots: &Roots, force: bool, dry_run: bool) -> Result<Restored> {
        let _lock = lock(&roots.data.join(".profile-restore.lock"))?;
        let mut planned = Vec::new();
        let mut in_place = Vec::new();
        let mut unique = BTreeSet::new();
        let mut existing = Vec::new();
        for entry in &self.manifest.entries {
            ensure!(
                allowed(&entry.path, self.manifest.scope),
                "invalid profile target"
            );
            let target = target(roots, &entry.path)?;
            ensure!(unique.insert(target.clone()), "profile targets overlap");
            // Nothing is ever written through a link; one that stays inside
            // the folder and already leads to these bytes is left alone.
            if check_path(&target).is_err() && self.in_place(roots, entry, &target) {
                in_place.push(target);
                continue;
            }
            check_path(&target)?;
            if let Ok(metadata) = fs::symlink_metadata(&target) {
                ensure!(
                    metadata.is_file(),
                    "profile target is not a regular file: {}",
                    target.display()
                );
                existing.push(target.display().to_string());
            }
            for parent in target.ancestors().skip(1) {
                if let Ok(metadata) = fs::symlink_metadata(parent) {
                    ensure!(
                        metadata.is_dir(),
                        "profile parent is not a directory: {}",
                        parent.display()
                    );
                }
            }
            planned.push((entry, target));
        }
        let targets = || planned.iter().map(|(_, target)| target.clone()).collect();
        // A dry run reports every file it would overwrite instead of
        // stopping at the first one.
        if dry_run {
            return Ok(Restored {
                targets: targets(),
                in_place,
            });
        }
        ensure!(
            force || existing.is_empty(),
            "profile restore would overwrite {}; use --force",
            existing.join(", ")
        );
        let mut staged = Vec::new();
        let preparation = (|| -> Result<()> {
            for (entry, target) in &planned {
                let parent = target.parent().context("profile target has no parent")?;
                private_dir(parent)?;
                let file = parent.join(format!(".haarness-profile-{}.tmp", Uuid::now_v7()));
                staged.push((
                    file.clone(),
                    target.clone(),
                    parent.join(format!(".haarness-profile-{}.bak", Uuid::now_v7())),
                ));
                let data = self
                    .files
                    .get(&entry.path)
                    .context("profile file missing")?;
                let data = if is_machine_settings(&entry.path) {
                    with_machine_settings(data, machine_settings(target))
                } else {
                    data.clone()
                };
                private_write(&file, &data, entry.executable)?;
            }
            Ok(())
        })();
        if let Err(error) = preparation {
            for (file, _, _) in &staged {
                let _ = fs::remove_file(file);
            }
            return Err(error);
        }
        let mut applied = Vec::new();
        let result = (|| -> Result<()> {
            for (file, target, backup) in &staged {
                check_path(target)?;
                let existed = target.exists();
                ensure!(!existed || force, "profile target appeared during restore");
                if existed {
                    ensure!(
                        fs::symlink_metadata(target)?.is_file(),
                        "profile target changed to a non-regular file"
                    );
                    fs::rename(target, backup)?;
                }
                applied.push((target, backup, existed));
                fs::rename(file, target)?;
            }
            Ok(())
        })();
        if result.is_err() {
            for (target, backup, existed) in applied.into_iter().rev() {
                let _ = fs::remove_file(target);
                if existed {
                    fs::rename(backup, target).with_context(|| {
                        format!(
                            "restore rollback failed; original preserved at {}",
                            backup.display()
                        )
                    })?;
                }
            }
        }
        for (file, _, backup) in staged {
            let _ = fs::remove_file(file);
            if result.is_ok() {
                let _ = fs::remove_file(backup);
            }
        }
        result?;
        Ok(Restored {
            targets: targets(),
            in_place,
        })
    }

    /// Whether `target`, which a link leads through, already holds what
    /// `entry` restores: the link stays inside the folder the entry restores
    /// into, and the regular file it leads to has the same bytes.
    fn in_place(&self, roots: &Roots, entry: &Entry, target: &Path) -> bool {
        let Some(root) = root(roots, &entry.path) else {
            return false;
        };
        let (Ok(root), Ok(real)) = (fs::canonicalize(root), fs::canonicalize(target)) else {
            return false;
        };
        let Some(data) = self.files.get(&entry.path) else {
            return false;
        };
        if !real.starts_with(&root) || !fs::symlink_metadata(&real).is_ok_and(|m| m.is_file()) {
            return false;
        }
        let data = if is_machine_settings(&entry.path) {
            with_machine_settings(data, machine_settings(target))
        } else {
            data.clone()
        };
        let mut held = Vec::new();
        File::open(&real)
            .and_then(|file| file.take(data.len() as u64 + 1).read_to_end(&mut held))
            .is_ok_and(|_| held == data)
    }
}

/// Global configuration files that may hold this machine's usage-statistics
/// choice (`telemetry.usage`).
fn is_machine_settings(path: &str) -> bool {
    matches!(
        path,
        "global/config/haarness.json" | "global/config/haarness.jsonc"
    )
}

/// The `telemetry` setting in an existing configuration file.
fn machine_settings(path: &Path) -> Option<serde_json::Value> {
    let text = fs::read_to_string(path).ok()?;
    jsonc_parser::parse_to_serde_value::<serde_json::Value>(
        &text,
        &jsonc_parser::ParseOptions::default(),
    )
    .ok()?
    .get("telemetry")
    .cloned()
}

/// Consent to usage statistics belongs to one machine: a profile never
/// carries it, and restoring one keeps the target's own choice. This is
/// `data` with its `telemetry` setting replaced by `kept`, or removed;
/// comments and formatting stay. Text that is not a JSON object is left as is.
fn with_machine_settings(data: &[u8], kept: Option<serde_json::Value>) -> Vec<u8> {
    use jsonc_parser::cst::{CstInputValue, CstRootNode};
    fn input(value: serde_json::Value) -> CstInputValue {
        match value {
            serde_json::Value::Null => CstInputValue::Null,
            serde_json::Value::Bool(value) => CstInputValue::Bool(value),
            serde_json::Value::Number(value) => CstInputValue::Number(value.to_string()),
            serde_json::Value::String(value) => CstInputValue::String(value),
            serde_json::Value::Array(values) => {
                CstInputValue::Array(values.into_iter().map(input).collect())
            }
            serde_json::Value::Object(values) => CstInputValue::Object(
                values
                    .into_iter()
                    .map(|(key, value)| (key, input(value)))
                    .collect(),
            ),
        }
    }
    let Ok(text) = std::str::from_utf8(data) else {
        return data.to_vec();
    };
    let Ok(root) = CstRootNode::parse(text, &jsonc_parser::ParseOptions::default()) else {
        return data.to_vec();
    };
    let Some(object) = root.object_value() else {
        return data.to_vec();
    };
    let current = object.get("telemetry");
    if current.is_none() && kept.is_none() {
        return data.to_vec();
    }
    if let Some(property) = current {
        property.remove();
    }
    if let Some(value) = kept {
        object.append("telemetry", input(value));
    }
    root.to_string().into_bytes()
}

fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.trim().is_empty() && name.len() <= 200 && !name.chars().any(char::is_control),
        "profile name must be 1–200 bytes without control characters"
    );
    Ok(())
}

fn target(roots: &Roots, path: &str) -> Result<PathBuf> {
    let (root, relative) = split(roots, path).context("invalid profile target")?;
    Ok(root.join(relative))
}

/// The folder an archive path restores into.
fn root<'a>(roots: &'a Roots, path: &str) -> Option<&'a Path> {
    split(roots, path).map(|(root, _)| root)
}

fn split<'a, 'b>(roots: &'a Roots, path: &'b str) -> Option<(&'a Path, &'b str)> {
    [
        ("global/config/", &roots.config),
        ("global/data/", &roots.data),
        ("project/", &roots.project),
    ]
    .into_iter()
    .find_map(|(prefix, root)| {
        path.strip_prefix(prefix)
            .map(|relative| (root.as_path(), relative))
    })
}

type Captured = BTreeMap<String, (Vec<u8>, bool)>;

/// How many symbolic links one save follows at most, so links that fan out
/// into one another cannot keep a save walking the same folders.
const MAX_LINKS: usize = 1000;

/// How far inside its folder a symbolic link may lead and still be saved.
#[derive(Clone, Copy)]
enum Reach {
    /// Anywhere: an installed plugin's folder is saved whole.
    Whole,
    /// Only to what a profile saves from the folder: the configuration and
    /// data folders also hold sign-ins, history and other state of this
    /// computer.
    Saved,
    /// To what a profile saves from the project, or to a Markdown file in it
    /// (an `AGENTS.md` kept as `CLAUDE.md`). A project also holds files no
    /// profile carries, such as untracked secrets, and may be the home folder.
    SavedOrMarkdown,
}

/// A folder a profile saves from. A symbolic link met while saving it is
/// saved as what it leads to only when that is inside this folder and within
/// its reach, and so is every file the save opens.
struct Place<'a> {
    /// The folder's real path, every link on the way to it resolved.
    real: PathBuf,
    /// The archive path its contents are saved under.
    prefix: &'a str,
    reach: Reach,
}

/// What a save does with a symbolic link.
enum Link {
    /// Save what it leads to, this real path, under the link's own name.
    Follow(PathBuf),
    /// Pass over it without a word: it leads nowhere, or back into a folder
    /// the save is inside, whose files are saved where they are.
    Skip,
    /// Leave it out and say so: it leads to files a profile does not save.
    LeftOut,
}

/// What one save keeps track of while it walks.
#[derive(Default)]
struct Walk {
    left_out: Vec<PathBuf>,
    followed: usize,
    used: usize,
    /// The real folders holding the folder links being followed, outermost
    /// first: a link back to one of them, or above, would never end.
    holding: Vec<PathBuf>,
}

impl<'a> Place<'a> {
    fn new(folder: &Path, prefix: &'a str, reach: Reach) -> Self {
        Self {
            real: dunce::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf()),
            prefix,
            reach,
        }
    }

    /// Where `real`, a real path, is inside this folder, as an archive-style
    /// relative path; none when it is outside.
    fn inside(&self, real: &Path) -> Option<String> {
        real.strip_prefix(&self.real)
            .ok()?
            .to_str()
            .map(|relative| relative.replace('\\', "/"))
    }

    /// Whether `real`, at `relative` inside this folder, is within its reach.
    fn reaches(&self, relative: &str, real: &Path, scope: Scope) -> bool {
        if relative.is_empty() {
            return true;
        }
        // `.env` files, keys, credential stores and dependencies stay out
        // under any name.
        if excluded(relative) {
            return false;
        }
        let saved = format!("{}/{relative}", self.prefix);
        let portable = allowed(&saved, scope) || allowed(&format!("{saved}/_"), scope);
        match self.reach {
            Reach::Whole => true,
            Reach::Saved => portable,
            Reach::SavedOrMarkdown => {
                portable
                    || PROJECT_DIRS.contains(&relative)
                    || (Path::new(relative)
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
                        && real.is_file())
            }
        }
    }

    /// The verdict for `path`: a link, or a saved folder that is, or sits
    /// under, one.
    fn judge(&self, path: &Path, scope: Scope, holding: &[PathBuf]) -> Result<Link> {
        let target = match dunce::canonicalize(path) {
            Ok(target) => target,
            Err(_) if path.is_symlink() && fs::metadata(path).is_err() => return Ok(Link::Skip),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("profile source cannot be resolved: {}", path.display())
                });
            }
        };
        let Some(relative) = self.inside(&target) else {
            return Ok(Link::LeftOut);
        };
        let parent = path
            .parent()
            .and_then(|parent| dunce::canonicalize(parent).ok());
        if parent
            .iter()
            .chain(holding)
            .any(|folder| folder.starts_with(&target))
        {
            return Ok(Link::Skip);
        }
        Ok(if self.reaches(&relative, &target, scope) {
            Link::Follow(target)
        } else {
            Link::LeftOut
        })
    }
}

fn collect(
    root: &Path,
    prefix: &str,
    place: &Place,
    scope: Scope,
    files: &mut Captured,
    walk: &mut Walk,
) -> Result<()> {
    match fs::symlink_metadata(root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    walk.used = files.values().map(|(bytes, _)| bytes.len()).sum();
    // A saved folder that is itself a link, or sits under one, follows the
    // same rule as a link inside it.
    match place.judge(root, scope, &[])? {
        Link::Follow(real) => save(&real, root, prefix, place, scope, files, walk),
        Link::Skip => Ok(()),
        Link::LeftOut => {
            walk.left_out.push(root.to_path_buf());
            Ok(())
        }
    }
}

/// Save what the real folder or file `real` holds under the archive path
/// `prefix`; `shown` is where the person sees it. Links are never followed
/// by the walk itself, so no linked folder is opened before it is judged.
fn save(
    real: &Path,
    shown: &Path,
    prefix: &str,
    place: &Place,
    scope: Scope,
    files: &mut Captured,
    walk: &mut Walk,
) -> Result<()> {
    for entry in walkdir::WalkDir::new(real)
        .follow_links(false)
        .follow_root_links(false)
        .into_iter()
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            if entry.file_name().to_str().is_some_and(macos_clutter) {
                return false;
            }
            let relative = entry.path().strip_prefix(real).unwrap_or(Path::new(""));
            let path = format!("{prefix}/{}", relative.to_string_lossy().replace('\\', "/"));
            allowed(&path, scope) || allowed(&format!("{path}/_"), scope)
        })
    {
        let entry = entry?;
        let relative = entry.path().strip_prefix(real)?;
        let suffix = relative
            .to_str()
            .context("profile filename must be UTF-8")?
            .replace('\\', "/");
        let (path, named) = if suffix.is_empty() {
            (prefix.to_owned(), shown.to_path_buf())
        } else {
            (format!("{prefix}/{suffix}"), shown.join(relative))
        };
        if !allowed(&path, scope) && !allowed(&format!("{path}/_"), scope) {
            continue;
        }
        if entry.file_type().is_symlink() {
            match place.judge(entry.path(), scope, &walk.holding)? {
                // A file where a profile saves only folders would not be a
                // portable archive.
                Link::Follow(target) if target.is_file() && !allowed(&path, scope) => {
                    walk.left_out.push(named);
                }
                Link::Follow(target) => {
                    walk.followed += 1;
                    ensure!(
                        walk.followed <= MAX_LINKS,
                        "profile has more than {MAX_LINKS} symbolic links to follow"
                    );
                    walk.holding
                        .push(entry.path().parent().unwrap_or(real).to_path_buf());
                    let saved = save(&target, &named, &path, place, scope, files, walk);
                    walk.holding.pop();
                    saved?;
                }
                Link::Skip => {}
                Link::LeftOut => walk.left_out.push(named),
            }
            continue;
        }
        if entry.file_type().is_dir() {
            continue;
        }
        ensure!(entry.file_type().is_file(), "profile source is not regular");
        let file = open(entry.path())?;
        // Judge the file that was opened rather than its path, so a link put
        // in place since the path was walked cannot bring in another file.
        let opened = held(&file, entry.path());
        if !opened.is_some_and(|opened| {
            place
                .inside(&opened)
                .is_some_and(|relative| place.reaches(&relative, &opened, scope))
        }) {
            walk.left_out.push(named);
            continue;
        }
        let metadata = file.metadata()?;
        ensure!(metadata.is_file(), "profile source is not regular");
        walk.used -= files.get(&path).map_or(0, |(bytes, _)| bytes.len());
        ensure!(
            metadata.len() <= (MAX_EXPANDED - walk.used) as u64,
            "profile exceeds archive limits"
        );
        let mut data = Vec::new();
        (&file)
            .take((MAX_EXPANDED - walk.used) as u64 + 1)
            .read_to_end(&mut data)?;
        ensure!(
            walk.used + data.len() <= MAX_EXPANDED,
            "profile exceeds expanded limit"
        );
        #[cfg(unix)]
        let executable = {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let executable = false;
        walk.used += data.len();
        files.insert(path, (data, executable));
    }
    Ok(())
}

/// Open `path` to read it, without waiting on a named pipe put in its place.
fn open(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOCTTY);
    }
    options.open(path)
}

/// The real path of the file `opened` holds, as the system names it, which
/// a link put in place of `path` since it was opened does not change. Where
/// the system cannot say (Windows, Linux without `/proc`), the real path of
/// `path` while it still names that very file; otherwise none.
fn held(opened: &File, path: &Path) -> Option<PathBuf> {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        let mut real = PathBuf::new();
        if nix::fcntl::fcntl(opened, nix::fcntl::FcntlArg::F_GETPATH(&mut real)).is_ok() {
            return Some(real);
        }
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        use std::os::fd::AsRawFd as _;
        if let Ok(real) = fs::read_link(format!("/proc/self/fd/{}", opened.as_raw_fd())) {
            return Some(real);
        }
    }
    let real = dunce::canonicalize(path).ok()?;
    still_names(opened, &real).then_some(real)
}

#[cfg(unix)]
fn still_names(opened: &File, real: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (opened.metadata(), fs::metadata(real)) {
        (Ok(opened), Ok(named)) => (opened.dev(), opened.ino()) == (named.dev(), named.ino()),
        _ => false,
    }
}

#[cfg(not(unix))]
fn still_names(opened: &File, real: &Path) -> bool {
    match (
        opened.try_clone().and_then(same_file::Handle::from_file),
        same_file::Handle::from_path(real),
    ) {
        (Ok(opened), Ok(named)) => opened == named,
        _ => false,
    }
}

fn private_dir(path: &Path) -> Result<()> {
    check_path(path)?;
    if path.exists() {
        return Ok(());
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(())
}

fn private_write(path: &Path, bytes: &[u8], executable: bool) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(if executable { 0o700 } else { 0o600 });
    }
    #[cfg(not(unix))]
    let _ = executable;
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn lock(path: &Path) -> Result<File> {
    let file = lockfile(path)?;
    file.lock_exclusive()?;
    Ok(file)
}

/// Hold across runtime configuration composition so a restore cannot expose a partial generation.
pub fn configuration_lock(data: &Path) -> Result<File> {
    let file = lockfile(&dunce::canonicalize(data)?.join(".profile-restore.lock"))?;
    FileExt::lock_shared(&file)?;
    Ok(file)
}

fn lockfile(path: &Path) -> Result<File> {
    let parent = path.parent().context("lock has no parent")?;
    private_dir(parent)?;
    check_path(path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    Ok(file)
}

pub fn write(path: &Path, bytes: &[u8], force: bool) -> Result<()> {
    check_path(path)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    private_dir(parent)?;
    ensure!(
        !path.exists() || force,
        "profile output already exists; use --force"
    );
    let temp = parent.join(format!(".haarness-profile-{}.tmp", Uuid::now_v7()));
    private_write(&temp, bytes, false)?;
    let result = if force {
        fs::rename(&temp, path)
    } else {
        fs::hard_link(&temp, path)
    };
    let _ = fs::remove_file(&temp);
    result.context("publish profile archive")
}

#[derive(Clone)]
pub struct Catalog {
    root: PathBuf,
}

impl Catalog {
    pub fn new(data: &Path) -> Self {
        Self {
            root: dunce::canonicalize(data)
                .unwrap_or_else(|_| data.to_path_buf())
                .join("profiles"),
        }
    }

    pub fn put(&self, bytes: &[u8]) -> Result<Record> {
        let archive = Archive::decode(bytes)?;
        let _lock = lock(&self.root.join(".lock"))?;
        ensure!(
            fs::read_dir(&self.root)?
                .filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().parse::<Uuid>().is_ok())
                .count()
                < 1000,
            "profile catalog is full (1000 archives)"
        );
        let id = Uuid::now_v7();
        let record = record(id, &archive.manifest, bytes);
        let stage = self.root.join(format!(".upload-{id}"));
        private_dir(&stage)?;
        let result = (|| -> Result<()> {
            private_write(&stage.join("archive.zip"), bytes, false)?;
            private_write(
                &stage.join("metadata.json"),
                &serde_json::to_vec(&record)?,
                false,
            )?;
            fs::rename(&stage, self.root.join(id.to_string()))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&stage);
        }
        result?;
        Ok(record)
    }

    pub fn download(&self, id: Uuid) -> Result<Vec<u8>> {
        let path = self.root.join(id.to_string()).join("archive.zip");
        check_path(&path)?;
        let bytes = read(&path)?;
        let record = self.get(id)?;
        ensure!(
            bytes.len() == record.size_bytes && hash(&bytes) == record.sha256,
            "stored profile checksum mismatch"
        );
        Ok(bytes)
    }

    pub fn get(&self, id: Uuid) -> Result<Record> {
        let path = self.root.join(id.to_string()).join("metadata.json");
        check_path(&path)?;
        let mut bytes = Vec::new();
        File::open(path)?
            .take(16 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 16 * 1024, "profile metadata too large");
        let record: Record = serde_json::from_slice(&bytes)?;
        ensure!(record.id == id, "profile metadata identity mismatch");
        Ok(record)
    }

    pub fn list(&self, query: &str, offset: usize, limit: usize) -> Result<Vec<Record>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        check_path(&self.root)?;
        let mut ids = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if let Ok(id) = entry.file_name().to_string_lossy().parse::<Uuid>() {
                ids.push(id);
            }
        }
        ids.sort_unstable_by(|a, b| b.cmp(a));
        let mut records = Vec::new();
        let query = query.to_lowercase();
        let mut matched = 0;
        for id in ids {
            let record = match self.get(id) {
                Ok(record) => record,
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
            if !record.name.to_lowercase().contains(&query) {
                continue;
            }
            if matched >= offset && records.len() < limit {
                records.push(record);
            }
            matched += 1;
            if records.len() == limit {
                break;
            }
        }
        Ok(records)
    }

    pub fn remove(&self, id: Uuid) -> Result<()> {
        let _lock = lock(&self.root.join(".lock"))?;
        let path = self.root.join(id.to_string());
        check_path(&path)?;
        fs::remove_dir_all(path)?;
        Ok(())
    }
}

fn record(id: Uuid, manifest: &Manifest, bytes: &[u8]) -> Record {
    let uploaded_at = id
        .get_timestamp()
        .and_then(|t| {
            let (seconds, nanos) = t.to_unix();
            i64::try_from(seconds)
                .ok()
                .and_then(|seconds| DateTime::from_timestamp(seconds, nanos))
        })
        .unwrap_or(manifest.created_at);
    Record {
        id,
        name: manifest.name.clone(),
        scope: manifest.scope,
        created_at: manifest.created_at,
        uploaded_at,
        entry_count: manifest.entries.len(),
        size_bytes: bytes.len(),
        sha256: hash(bytes),
    }
}
