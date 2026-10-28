//! Shared storage planes and scoped resource discovery for every native client.
use anyhow::{Context, Result, bail};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub struct Layout {
    pub config: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
    pub state: PathBuf,
}
impl Layout {
    pub fn resolve(
        home: &Path,
        root: Option<&Path>,
        env: impl Fn(&str) -> Option<PathBuf>,
    ) -> Result<Self> {
        if let Some(root) = root {
            if !root.is_absolute() {
                bail!("HAARNESS_HOME must be an absolute path");
            }
            return Ok(Self {
                config: root.join("config"),
                data: root.join("data"),
                cache: root.join("cache"),
                state: root.join("state"),
            });
        }
        let plane = |key, fallback: &str| {
            env(key)
                .filter(|path| path.is_absolute())
                .unwrap_or_else(|| home.join(fallback))
                .join("haarness")
        };
        Ok(Self {
            config: plane("XDG_CONFIG_HOME", ".config"),
            data: plane("XDG_DATA_HOME", ".local/share"),
            cache: plane("XDG_CACHE_HOME", ".cache"),
            state: plane("XDG_STATE_HOME", ".local/state"),
        })
    }

    pub fn initialize(&self, legacy: Option<&directories::ProjectDirs>) -> Result<()> {
        // Existing daemon locks and SQLite WAL files must retain their identity.
        // Alias the existing data/cache directories rather than copying a live DB.
        if let Some(legacy) = legacy {
            adopt_directory(&self.data, legacy.data_local_dir())?;
            adopt_directory(&self.cache, legacy.cache_dir())?;
        }
        for dir in [&self.config, &self.data, &self.cache, &self.state] {
            fs::create_dir_all(dir)?;
        }
        let marker = self.config.join(".layout-v1");
        if !marker.exists() {
            if let Some(legacy) = legacy {
                for name in [
                    "config.toml",
                    "haarness.json",
                    "haarness.jsonc",
                    "opencode.json",
                    "opencode.jsonc",
                    "AGENTS.md",
                    "agents",
                    "agent",
                    "commands",
                    "command",
                    "skills",
                    "extensions",
                    "extensions.json",
                    "output-styles",
                ] {
                    copy_missing(&legacy.config_dir().join(name), &self.config.join(name))?;
                }
                copy_missing(
                    &legacy.data_local_dir().join("skills"),
                    &self.config.join("skills"),
                )?;
                copy_missing(
                    &legacy.data_local_dir().join("plugins/local"),
                    &self.config.join("plugins/local"),
                )?;
                copy_missing(
                    &legacy.config_dir().join("auth.json"),
                    &self.data.join("auth.json"),
                )?;
            }
            fs::write(
                marker,
                "Configuration copied without overwriting existing files. Legacy data retained.\n",
            )?;
        }
        let workflows = self.config.join(".workflows-v2");
        if !workflows.exists() {
            match self.adopt_legacy_workflows(legacy) {
                Ok(()) => fs::write(
                    workflows,
                    "Global workflows taken from the legacy folder; the folder they replaced is kept as workflows-before-move. Legacy files retained.\n",
                )?,
                // Starting matters more than moving them: the next start tries again.
                Err(error) => eprintln!(
                    "haarness: global workflows were not moved to {} yet: {error:#}",
                    self.config.join("workflows").display()
                ),
            }
        }
        for name in [
            "agents",
            "commands",
            "skills",
            "plugins/local",
            "extensions",
        ] {
            fs::create_dir_all(self.config.join(name))?;
        }
        Ok(())
    }
}

impl Layout {
    /// Until the workflow catalog read `workflows` here, it read the legacy
    /// folder's, so that one holds the global workflows people have, and the
    /// one here was never read where the two differ. The legacy one is copied
    /// into a fresh folder that takes this one's place; this one is kept beside
    /// it as `workflows-before-move`. Nothing changes when they are the same
    /// folder (Linux, or one links to the other), and a failure leaves both
    /// as they were.
    fn adopt_legacy_workflows(&self, legacy: Option<&directories::ProjectDirs>) -> Result<()> {
        legacy.map_or(Ok(()), |legacy| {
            self.adopt_workflows(&legacy.config_dir().join("workflows"))
        })
    }

    fn adopt_workflows(&self, source: &Path) -> Result<()> {
        let target = self.config.join("workflows");
        if !source.is_dir() {
            return Ok(());
        }
        if fs::canonicalize(source).ok() == fs::canonicalize(&target).ok() && target.exists() {
            return Ok(());
        }
        let fresh = self
            .config
            .join(format!(".workflows-moving-{}", uuid::Uuid::now_v7()));
        let result = (|| -> Result<()> {
            copy_tree(source, &fresh)?;
            if fs::symlink_metadata(&target).is_ok() {
                let mut aside = self.config.join("workflows-before-move");
                if fs::symlink_metadata(&aside).is_ok() {
                    aside = self
                        .config
                        .join(format!("workflows-before-move-{}", uuid::Uuid::now_v7()));
                }
                fs::rename(&target, aside)?;
            }
            fs::rename(&fresh, &target)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&fresh);
        }
        result.with_context(|| format!("copy {}", source.display()))
    }
}

/// `source`'s files and folders into the new folder `target`. A link stays a
/// link to what it names, as [`copy_missing`] keeps it; one that names nothing
/// is left out, since nothing could read it.
fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let to = target.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            #[cfg(unix)]
            if let Ok(resolved) = entry.path().canonicalize() {
                std::os::unix::fs::symlink(resolved, &to)?;
            }
        } else if kind.is_dir() {
            copy_tree(&entry.path(), &to)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

fn adopt_directory(target: &Path, source: &Path) -> Result<()> {
    if target == source || target.exists() || !source.exists() {
        return Ok(());
    }
    fs::create_dir_all(target.parent().context("storage parent")?)?;
    #[cfg(unix)]
    match std::os::unix::fs::symlink(source, target) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error).context("preserve existing Haarness data"),
    }
    #[cfg(not(unix))]
    {
        // A directory junction requires no developer-mode/symlink privilege.
        #[cfg(windows)]
        {
            let result = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(target)
                .arg(source)
                .output()?;
            if !result.status.success() {
                bail!(
                    "could not preserve existing Haarness directory at {}",
                    source.display()
                );
            }
            Ok(())
        }
        #[cfg(not(windows))]
        bail!(
            "existing data must be migrated from {} to {}",
            source.display(),
            target.display()
        )
    }
}

pub fn copy_missing(source: &Path, target: &Path) -> Result<()> {
    if source == target || !source.exists() {
        return Ok(());
    }
    if source.is_dir() {
        if target.exists() && !target.is_dir() {
            return Ok(());
        }
        fs::create_dir_all(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            // Preserve user-managed symlinks without following directory cycles.
            if entry.file_type()?.is_symlink() {
                #[cfg(unix)]
                if !target.join(entry.file_name()).exists() {
                    let resolved = entry.path().canonicalize()?;
                    match std::os::unix::fs::symlink(resolved, target.join(entry.file_name())) {
                        Ok(()) => (),
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
                        Err(error) => return Err(error.into()),
                    }
                }
                continue;
            }
            copy_missing(&entry.path(), &target.join(entry.file_name()))?;
        }
    } else {
        if target.exists() {
            return Ok(());
        }
        fs::create_dir_all(target.parent().context("configuration parent")?)?;
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let temporary = target.with_extension(format!("{}.migrating", uuid::Uuid::now_v7()));
        let result = (|| -> Result<()> {
            let mut out = options.open(&temporary)?;
            io::copy(&mut fs::File::open(source)?, &mut out)?;
            out.sync_all()?;
            match fs::hard_link(&temporary, target) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
                Err(error) => Err(error.into()),
            }
        })();
        let _ = fs::remove_file(temporary);
        result?;
    }
    Ok(())
}

/// Project layers, oldest ancestor first, bounded by the enclosing worktree.
/// Outside Git, the selected workspace itself is the project boundary.
pub fn project_directories(workspace: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for directory in workspace.ancestors() {
        dirs.push(directory.to_path_buf());
        if directory.join(".git").exists() {
            dirs.reverse();
            return dirs;
        }
    }
    vec![workspace.to_path_buf()]
}

/// The system configuration folder this process reads
/// (`HAARNESS_SYSTEM_CONFIG_DIR`, absolute, see
/// [`haarness_protocol::system_config_dir`]). This crate's own tests never read
/// one from the machine they run on: a test that needs a system folder passes
/// it ([`SystemFolder::at`], `jsonconfig::load_layers_with_system`).
#[cfg(not(test))]
pub fn system_config_dir() -> Option<PathBuf> {
    haarness_protocol::system_config_dir()
}

#[cfg(test)]
pub fn system_config_dir() -> Option<PathBuf> {
    None
}

/// The real roots of `workspace`'s project, where a cloned repository puts
/// what it likes: its worktree root and the workspace itself.
pub fn project_roots(workspace: &Path) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = [
        project_directories(workspace).first().cloned(),
        Some(workspace.to_path_buf()),
    ]
    .into_iter()
    .flatten()
    .filter_map(|path| path.canonicalize().ok())
    .collect();
    roots.dedup();
    roots
}

/// Whether `path`, where links really lead it, is inside one of `roots` (real
/// paths). A path that does not exist is inside none.
pub fn real_path_within(path: &Path, roots: &[PathBuf]) -> bool {
    !roots.is_empty()
        && path
            .canonicalize()
            .is_ok_and(|real| roots.iter().any(|root| real.starts_with(root)))
}

/// The system configuration folder (`HAARNESS_SYSTEM_CONFIG_DIR`) as one
/// workspace reads it: the lowest configuration layer, below the global
/// folder, which a team or device management ships. Haarness only reads it,
/// and never through a link that leads into the project, where a cloned
/// repository puts what it likes. A system folder the person points into the
/// project stays theirs, so everything in it is read.
#[derive(Clone, Debug)]
pub struct SystemFolder {
    root: PathBuf,
    /// The project's real roots, unless the folder itself is inside them.
    project: Vec<PathBuf>,
}

impl SystemFolder {
    /// The folder `HAARNESS_SYSTEM_CONFIG_DIR` names, for `workspace`; `None`
    /// when the variable is unset or blank.
    pub fn for_workspace(workspace: &Path) -> Option<Self> {
        system_config_dir().map(|root| Self::at(root, workspace))
    }

    pub fn at(root: PathBuf, workspace: &Path) -> Self {
        let mut project = project_roots(workspace);
        if real_path_within(&root, &project) {
            project.clear();
        }
        Self { root, project }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether `path` really is inside the project, through a link.
    pub fn leads_into_project(&self, path: &Path) -> bool {
        real_path_within(path, &self.project)
    }

    /// Whether `path`, an entry found in the folder, is left out because a link
    /// leads it into the project; logged when it is.
    pub fn excluded(&self, path: &Path) -> bool {
        let excluded = self.leads_into_project(path);
        if excluded {
            tracing::warn!(
                path = %path.display(),
                "config: system folder entry not read: a link leads it into the project"
            );
        }
        excluded
    }

    /// The folders under the folder's `name` that hold a `SKILL.md`, each read
    /// where it is, leaving out every one a link leads into the project.
    pub fn skill_folders(&self, name: &str) -> Vec<PathBuf> {
        let mut folders: Vec<PathBuf> = self
            .files(name)
            .into_iter()
            .filter(|file| file.file_name().is_some_and(|file| file == "SKILL.md"))
            .filter_map(|file| file.parent().map(Path::to_path_buf))
            .collect();
        folders.dedup();
        folders
    }

    /// `name` in the folder, unless a link leads it into the project (which is
    /// logged and left out).
    pub fn entry(&self, name: &str) -> Option<PathBuf> {
        let path = self.root.join(name);
        (!self.excluded(&path)).then_some(path)
    }

    /// The files under the folder's `name`, at any depth, leaving out each one
    /// a link leads into the project.
    pub fn files(&self, name: &str) -> Vec<PathBuf> {
        fn walk(base: &SystemFolder, path: &Path, depth: usize, out: &mut Vec<PathBuf>) {
            if depth > 16 {
                return;
            }
            if base.excluded(path) {
                return;
            }
            if path.is_file() {
                out.push(path.to_path_buf());
                return;
            }
            let Ok(entries) = fs::read_dir(path) else {
                return;
            };
            let mut children: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
            children.sort();
            for child in children {
                walk(base, &child, depth + 1, out);
            }
        }
        let mut out = Vec::new();
        walk(self, &self.root.join(name), 0, &mut out);
        out
    }
}

pub fn resource_roots(
    config: &Path,
    home: Option<&Path>,
    workspace: &Path,
    project_config: bool,
) -> Vec<PathBuf> {
    resource_roots_scoped(config, home, workspace, project_config)
        .into_iter()
        .map(|(root, _)| root)
        .collect()
}

/// [`resource_roots`], each with whether it is one of the project's own
/// folders rather than the person's. A project folder that is the home
/// folder (a project, or a Git repository of dotfiles, at `~`) lists the
/// person's folders again: they stay the person's.
pub fn resource_roots_scoped(
    config: &Path,
    home: Option<&Path>,
    workspace: &Path,
    project_config: bool,
) -> Vec<(PathBuf, bool)> {
    let mut roots = Vec::new();
    if let Some(home) = home {
        roots.extend([".opencode", ".haarness"].map(|name| (home.join(name), false)));
    }
    roots.push((config.to_path_buf(), false));
    for directory in project_directories(workspace)
        .into_iter()
        .filter(|_| project_config)
    {
        for name in [".opencode", ".haarness"] {
            let root = directory.join(name);
            let project = !persons_folder(&roots, &root);
            roots.push((root, project));
        }
    }
    roots
}

/// Whether `folder` is one of `listed` that is the person's, as written or
/// where it really is.
pub(crate) fn persons_folder(listed: &[(PathBuf, bool)], folder: &Path) -> bool {
    let real = folder.canonicalize().ok();
    listed.iter().any(|(known, project)| {
        !project && (known == folder || real.is_some() && known.canonicalize().ok() == real)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn planes_follow_xdg_and_single_root_has_precedence() {
        let home = Path::new("/home/fixture");
        let layout = Layout::resolve(home, None, |key| {
            (key == "XDG_CONFIG_HOME").then(|| PathBuf::from("/config"))
        })
        .unwrap();
        assert_eq!(layout.config, Path::new("/config/haarness"));
        assert_eq!(layout.data, home.join(".local/share/haarness"));
        let isolated = Layout::resolve(home, Some(Path::new("/isolated")), |_| {
            Some(PathBuf::from("/ignored"))
        })
        .unwrap();
        assert_eq!(isolated.config, Path::new("/isolated/config"));
        assert_eq!(isolated.state, Path::new("/isolated/state"));
        assert!(Layout::resolve(home, Some(Path::new("relative")), |_| None).is_err());
    }
    #[test]
    fn migration_keeps_existing_files_and_live_data_identity() {
        let root = std::env::temp_dir().join(format!("haarness-layout-{}", uuid::Uuid::now_v7()));
        let old = root.join("legacy");
        let new = root.join("config");
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(old.join("haarness.json"), "old").unwrap();
        fs::write(new.join("haarness.json"), "current").unwrap();
        fs::write(old.join("auth.json"), "retained").unwrap();
        copy_missing(&old, &new).unwrap();
        copy_missing(&old, &new).unwrap();
        assert_eq!(
            fs::read_to_string(new.join("haarness.json")).unwrap(),
            "current"
        );
        assert_eq!(
            fs::read_to_string(new.join("auth.json")).unwrap(),
            "retained"
        );
        #[cfg(unix)]
        {
            let alias = root.join("data");
            adopt_directory(&alias, &old).unwrap();
            fs::write(old.join("active.db"), "live").unwrap();
            assert_eq!(fs::read_to_string(alias.join("active.db")).unwrap(), "live");
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn global_workflows_come_from_the_folder_that_was_read() {
        let root =
            std::env::temp_dir().join(format!("haarness-workflows-{}", uuid::Uuid::now_v7()));
        let layout = Layout::resolve(&root, Some(&root.join("home")), |_| None).unwrap();
        let legacy = root.join("legacy/workflows");
        let current = layout.config.join("workflows");
        fs::create_dir_all(legacy.join("nested")).unwrap();
        fs::create_dir_all(&current).unwrap();
        // The legacy folder was read: edited, added and deleted there since the
        // first copy. The current folder holds that stale copy.
        fs::write(legacy.join("review.ts"), "edited").unwrap();
        fs::write(legacy.join("nested/added.ts"), "added").unwrap();
        fs::write(current.join("review.ts"), "first copy").unwrap();
        fs::write(current.join("deleted.ts"), "deleted since").unwrap();
        let adopt = || layout.adopt_workflows(&legacy);
        adopt().unwrap();
        assert_eq!(
            fs::read_to_string(current.join("review.ts")).unwrap(),
            "edited"
        );
        assert_eq!(
            fs::read_to_string(current.join("nested/added.ts")).unwrap(),
            "added"
        );
        assert!(!current.join("deleted.ts").exists());
        let aside = layout.config.join("workflows-before-move");
        assert_eq!(
            fs::read_to_string(aside.join("deleted.ts")).unwrap(),
            "deleted since"
        );
        assert_eq!(
            fs::read_to_string(legacy.join("review.ts")).unwrap(),
            "edited"
        );
        // The same folder through a link is left alone, and no file is emptied.
        #[cfg(unix)]
        {
            fs::remove_dir_all(&current).unwrap();
            std::os::unix::fs::symlink(&legacy, &current).unwrap();
            adopt().unwrap();
            assert_eq!(
                fs::read_to_string(legacy.join("review.ts")).unwrap(),
                "edited"
            );
            assert!(
                fs::symlink_metadata(&current)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
        // A link that names nothing is left out instead of failing.
        #[cfg(unix)]
        {
            fs::remove_file(&current).unwrap();
            std::os::unix::fs::symlink(root.join("missing"), legacy.join("broken.ts")).unwrap();
            adopt().unwrap();
            assert!(fs::symlink_metadata(current.join("broken.ts")).is_err());
            assert_eq!(
                fs::read_to_string(current.join("review.ts")).unwrap(),
                "edited"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn discovery_stops_at_the_worktree() {
        let root = std::env::temp_dir().join(format!("haarness-layers-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(root.join("repo/.git")).unwrap();
        fs::create_dir_all(root.join("repo/sub")).unwrap();
        assert_eq!(
            project_directories(&root.join("repo/sub")),
            vec![root.join("repo"), root.join("repo/sub")]
        );
        assert_eq!(project_directories(&root), vec![root.clone()]);
        fs::remove_dir_all(root).unwrap();
    }
}
