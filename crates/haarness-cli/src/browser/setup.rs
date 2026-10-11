//! `haarness browser setup|status|remove`: register the bridge with Chromium
//! browsers and keep a copy of Haarness for Chrome where people can load it.
//!
//! Layout under `<data>/browser/chrome/`: the wrapper Chrome runs
//! (`haarness-browser-host`, `.bat` on Windows), `bin/` (a copy of the
//! executable when its own path is not stable), `Haarness for Chrome/` (the
//! unpacked extension), `ai.haarness.browser.json` (the manifest the Windows
//! registry names) and `removed` (people removed it; Desktop must not undo that).
use super::{
    BrowserChoice, CHROME_WEB_STORE_URL, EXTENSION_FOLDER, EXTENSION_ID, HOST_NAME, SetupArgs,
    discovery,
    manifest::{self, BROWSERS, Os, Place},
    relay,
};
use anyhow::{Context as _, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    fmt::Write as _,
    fs, io,
    path::{Path, PathBuf},
};

/// The files setup owns.
pub(crate) struct ChromeFiles {
    dir: PathBuf,
    os: Os,
}

impl ChromeFiles {
    pub(crate) fn new(data: &Path, os: Os) -> Self {
        Self {
            dir: data.join("browser").join("chrome"),
            os,
        }
    }
    pub(crate) fn wrapper(&self) -> PathBuf {
        self.dir.join(manifest::wrapper_file_name(self.os))
    }
    fn bin(&self) -> PathBuf {
        self.dir.join("bin")
    }
    pub(crate) fn extension(&self) -> PathBuf {
        self.dir.join(EXTENSION_FOLDER)
    }
    fn windows_manifest(&self) -> PathBuf {
        self.dir.join(manifest::manifest_file_name())
    }
    fn removed(&self) -> PathBuf {
        self.dir.join("removed")
    }
}

/// The Windows registry, behind a seam so the Windows flow runs in tests.
pub(crate) trait Registry {
    fn get(&self, key: &str) -> Option<String>;
    fn set(&self, key: &str, manifest: &Path) -> Result<()>;
    fn delete(&self, key: &str) -> Result<()>;
}

/// `reg.exe`.
struct RegExe;

impl RegExe {
    fn run(args: &[String]) -> Result<String> {
        let output = std::process::Command::new("reg")
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .context("cannot run reg.exe")?;
        if !output.status.success() {
            bail!(
                "reg {} failed: {}",
                args.first().map_or("", String::as_str),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

impl Registry for RegExe {
    fn get(&self, key: &str) -> Option<String> {
        if !cfg!(windows) {
            return None;
        }
        manifest::reg_default_value(&Self::run(&manifest::reg_query(key)).ok()?)
    }
    fn set(&self, key: &str, manifest: &Path) -> Result<()> {
        Self::run(&manifest::reg_add(key, manifest)).map(drop)
    }
    fn delete(&self, key: &str) -> Result<()> {
        Self::run(&manifest::reg_delete(key)).map(drop)
    }
}

/// Everything setup reads from the computer, gathered once.
pub(crate) struct Context<'a> {
    pub(crate) os: Os,
    pub(crate) home: PathBuf,
    pub(crate) local_app_data: Option<PathBuf>,
    pub(crate) data: PathBuf,
    /// This executable, canonical.
    pub(crate) exe: PathBuf,
    pub(crate) version: String,
    /// Running as a Linux AppImage, whose files vanish when it exits.
    pub(crate) appimage: bool,
    /// The variables the wrapper passes on.
    pub(crate) forwarded: Vec<(&'static str, String)>,
    /// A data folder other than the default was chosen.
    pub(crate) isolated: bool,
    pub(crate) extension_source: Option<PathBuf>,
    pub(crate) extra_ids: Vec<String>,
    /// For tests: `HAARNESS_BROWSER_MANIFEST_DIR` (a path list) stands in for
    /// every browser's own folder in setup, status and remove, as
    /// `--manifest-dir` does for setup, so a test never touches a real Chrome.
    pub(crate) manifest_dirs: Vec<PathBuf>,
    pub(crate) registry: &'a dyn Registry,
}

impl Context<'static> {
    fn from_environment(data: &Path, data_dir: Option<&Path>) -> Result<Self> {
        let exe = std::env::current_exe().context("cannot locate the Haarness executable")?;
        let exe = discovery::canonical(&exe);
        let home = discovery::home_dir().context("home directory unavailable")?;
        let var = |key: &str| std::env::var(key).ok().filter(|value| !value.is_empty());
        let data_dir =
            data_dir.map(|root| std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf()));
        let forwarded = manifest::FORWARDED
            .iter()
            .filter_map(|key| {
                let value = if *key == "HAARNESS_DATA_DIR" {
                    data_dir.as_ref().map(|root| root.display().to_string())
                } else {
                    var(key).filter(|value| Path::new(value).is_absolute())
                };
                value.map(|value| (*key, value))
            })
            .collect();
        Ok(Self {
            os: Os::current(),
            local_app_data: var("LOCALAPPDATA").map(PathBuf::from),
            data: data.to_path_buf(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            appimage: var("APPIMAGE").is_some(),
            forwarded,
            isolated: data_dir.is_some() || var("HAARNESS_HOME").is_some(),
            extension_source: extension_source(
                var("HAARNESS_BROWSER_EXTENSION_DIR").map(PathBuf::from),
                &exe,
            ),
            extra_ids: manifest::extension_ids(var("HAARNESS_BROWSER_EXTENSION_IDS").as_deref()),
            manifest_dirs: std::env::var_os("HAARNESS_BROWSER_MANIFEST_DIR")
                .map(|dirs| {
                    std::env::split_paths(&dirs)
                        .filter(|dir| !dir.as_os_str().is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            registry: &RegExe,
            exe,
            home,
        })
    }
}

/// Where the extension files to copy are: an explicit folder, Desktop's
/// bundle next to its `bin/`, or a development build of this repository.
pub(crate) fn extension_source(explicit: Option<PathBuf>, exe: &Path) -> Option<PathBuf> {
    let folder = exe.parent()?;
    explicit
        .into_iter()
        .chain([
            folder.join("../browser-extension/chrome"),
            folder.join("../../apps/browser-extension/dist/chrome"),
        ])
        .find(|candidate| candidate.join("manifest.json").is_file())
        .map(|candidate| discovery::canonical(&candidate))
}

/// The extension's `version_name` (its workspace version), else `version`.
pub(crate) fn extension_version(folder: &Path) -> Option<String> {
    let manifest: Value =
        serde_json::from_slice(&fs::read(folder.join("manifest.json")).ok()?).ok()?;
    manifest["version_name"]
        .as_str()
        .or_else(|| manifest["version"].as_str())
        .map(str::to_owned)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ExtensionCopy {
    pub(crate) path: String,
    pub(crate) version: String,
}

pub(crate) fn extension_copy(folder: &Path) -> Option<ExtensionCopy> {
    Some(ExtensionCopy {
        version: extension_version(folder)?,
        path: folder.display().to_string(),
    })
}

/// What the wrapper should run, and what to copy there first.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ExePlan {
    pub(crate) run: PathBuf,
    pub(crate) copy_from: Option<PathBuf>,
}

/// Run this executable where it is when its path lasts; copy it where it
/// does not (an AppImage's mount), or where it is locked while it runs and
/// updates must replace it (Windows).
pub(crate) fn plan_executable(
    exe: &Path,
    os: Os,
    appimage: bool,
    bin: &Path,
    version: &str,
) -> Result<ExePlan> {
    let text = exe.display().to_string();
    if os == Os::Mac && (text.contains("/AppTranslocation/") || text.starts_with("/Volumes/")) {
        bail!(
            "Haarness is running from {text}, which Chrome cannot rely on. Move Haarness to Applications, then run setup again."
        );
    }
    let copy = match os {
        Os::Windows => Some(bin.join(format!("haarness-{version}.exe"))),
        Os::Linux if appimage || text.starts_with("/tmp/.mount_") => Some(bin.join("haarness")),
        Os::Mac | Os::Linux => None,
    };
    Ok(match copy {
        Some(run) => ExePlan {
            run,
            copy_from: Some(exe.to_path_buf()),
        },
        None => ExePlan {
            run: exe.to_path_buf(),
            copy_from: None,
        },
    })
}

/// Whether the copy at `copy` still matches `source`.
fn copy_is_current(source: &Path, copy: &Path) -> bool {
    let (Ok(source), Ok(copy)) = (fs::metadata(source), fs::metadata(copy)) else {
        return false;
    };
    source.len() == copy.len()
        && matches!((source.modified(), copy.modified()), (Ok(source), Ok(copy)) if copy >= source)
}

/// For `--if-missing`: whether the wrapper must point somewhere new. A
/// wrapper that runs another working, installed Haarness is left alone.
pub(crate) fn wrapper_needs_rewrite(
    existing: Option<&Path>,
    plan: &ExePlan,
    exe: &Path,
    bin: &Path,
) -> bool {
    let Some(target) = existing else {
        return true;
    };
    if !target.exists() {
        return true;
    }
    if manifest::is_development_build(target) && !manifest::is_development_build(exe) {
        return true;
    }
    match &plan.copy_from {
        Some(source) if target.starts_with(bin) => {
            target != plan.run || !copy_is_current(source, &plan.run)
        }
        _ => false,
    }
}

/// The executable a wrapper on disk runs.
pub(crate) fn wrapper_exe(wrapper: &Path) -> Option<PathBuf> {
    manifest::wrapper_target(&fs::read_to_string(wrapper).ok()?)
}

fn manifest_points_at(manifest: &Path, wrapper: &Path) -> bool {
    fs::read(manifest)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .is_some_and(|manifest| manifest["path"].as_str() == Some(&wrapper.display().to_string()))
}

/// One browser (or folder) setup registers with.
struct Target {
    browser: String,
    name: String,
    page: &'static str,
    place: Place,
    detected: bool,
    note: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrowserEntry {
    pub(crate) browser: String,
    pub(crate) name: String,
    pub(crate) extensions_page: String,
    pub(crate) manifest: Option<String>,
    pub(crate) detected: bool,
    pub(crate) registered: bool,
    pub(crate) supported: bool,
    pub(crate) note: Option<String>,
}

/// The browsers to register with: the chosen ones, else Google Chrome and
/// every other one installed here (all of them, for `remove`). Sandboxed
/// Linux installs are listed as unsupported.
fn targets(
    context: &Context,
    choices: &[BrowserChoice],
    manifest_dirs: &[PathBuf],
    every: bool,
) -> Vec<Target> {
    if !manifest_dirs.is_empty() {
        return manifest_dirs
            .iter()
            .map(|folder| {
                let folder = std::path::absolute(folder).unwrap_or_else(|_| folder.clone());
                Target {
                    browser: "custom".into(),
                    name: folder.display().to_string(),
                    page: "chrome://extensions",
                    place: Place::File {
                        manifest: folder.join(manifest::manifest_file_name()),
                        profile: folder,
                    },
                    detected: true,
                    note: None,
                }
            })
            .collect();
    }
    let mut targets = Vec::new();
    for browser in BROWSERS {
        let place = manifest::place(
            browser,
            context.os,
            &context.home,
            context.local_app_data.as_deref(),
        );
        let detected = match &place {
            Place::File { profile, .. } | Place::Registry { profile, .. } => profile.exists(),
            Place::Unsupported => false,
        };
        let chosen = if choices.is_empty() {
            every || browser.choice == BrowserChoice::Chrome || detected
        } else {
            choices.contains(&browser.choice)
        };
        if !chosen || (every && place == Place::Unsupported) {
            continue;
        }
        let note = (place == Place::Unsupported).then(|| {
            format!(
                "Haarness for Chrome cannot register with {} on this computer yet.",
                browser.name
            )
        });
        targets.push(Target {
            browser: browser.key.into(),
            name: browser.name.into(),
            page: browser.extensions_page,
            place,
            detected,
            note,
        });
    }
    if context.os == Os::Linux && !every {
        for (key, name, folder) in manifest::SANDBOXED {
            let chosen = choices.is_empty()
                || choices
                    .iter()
                    .any(|choice| manifest::browser(*choice).key == *key);
            if chosen && context.home.join(folder).exists() {
                targets.push(Target {
                    browser: (*key).into(),
                    name: (*name).into(),
                    page: BROWSERS
                        .iter()
                        .find(|browser| browser.key == *key)
                        .map_or("chrome://extensions", |browser| browser.extensions_page),
                    place: Place::Unsupported,
                    detected: true,
                    note: Some(manifest::SANDBOXED_NOTE.into()),
                });
            }
        }
    }
    targets
}

impl Target {
    fn manifest(&self, files: &ChromeFiles) -> Option<PathBuf> {
        match &self.place {
            Place::File { manifest, .. } => Some(manifest.clone()),
            Place::Registry { .. } => Some(files.windows_manifest()),
            Place::Unsupported => None,
        }
    }

    /// The manifest exists, names the current wrapper, and its executable exists.
    fn registered(&self, files: &ChromeFiles, registry: &dyn Registry) -> bool {
        let wrapper = files.wrapper();
        let manifest = match &self.place {
            Place::File { manifest, .. } => manifest.clone(),
            Place::Registry { key, .. } => match registry.get(key) {
                Some(value) => PathBuf::from(value),
                None => return false,
            },
            Place::Unsupported => return false,
        };
        manifest_points_at(&manifest, &wrapper)
            && wrapper_exe(&wrapper).is_some_and(|exe| exe.exists())
    }

    fn entry(&self, files: &ChromeFiles, registry: &dyn Registry) -> BrowserEntry {
        BrowserEntry {
            browser: self.browser.clone(),
            name: self.name.clone(),
            extensions_page: self.page.into(),
            manifest: self.manifest(files).map(|path| path.display().to_string()),
            detected: self.detected,
            registered: self.registered(files, registry),
            supported: self.place != Place::Unsupported,
            note: self.note.clone(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Report {
    pub(crate) host_name: &'static str,
    pub(crate) extension_id: &'static str,
    pub(crate) web_store_url: Option<&'static str>,
    pub(crate) executable: String,
    pub(crate) wrapper: String,
    pub(crate) removed: bool,
    pub(crate) warnings: Vec<String>,
    pub(crate) browsers: Vec<BrowserEntry>,
    pub(crate) extension: Option<ExtensionCopy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) connections: Option<Vec<Value>>,
    /// What `remove` deleted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) deleted: Option<Vec<String>>,
}

fn report(
    context: &Context,
    files: &ChromeFiles,
    targets: &[Target],
    warnings: Vec<String>,
) -> Report {
    let wrapper = files.wrapper();
    let executable = wrapper_exe(&wrapper)
        .or_else(|| {
            plan_executable(
                &context.exe,
                context.os,
                context.appimage,
                &files.bin(),
                &context.version,
            )
            .ok()
            .map(|plan| plan.run)
        })
        .unwrap_or_else(|| context.exe.clone());
    Report {
        host_name: HOST_NAME,
        extension_id: EXTENSION_ID,
        web_store_url: CHROME_WEB_STORE_URL,
        executable: executable.display().to_string(),
        wrapper: wrapper.display().to_string(),
        removed: files.removed().exists(),
        warnings,
        browsers: targets
            .iter()
            .map(|target| target.entry(files, context.registry))
            .collect(),
        extension: extension_copy(&files.extension()),
        connections: None,
        deleted: None,
    }
}

pub(crate) struct SetupOptions<'a> {
    pub(crate) browsers: &'a [BrowserChoice],
    pub(crate) manifest_dirs: &'a [PathBuf],
    pub(crate) if_missing: bool,
}

/// Writes `contents` to `path` unless it already holds them, replacing it
/// whole so a running bridge never reads half a wrapper.
fn write_if_changed(path: &Path, contents: &[u8], executable: bool) -> Result<()> {
    if fs::read(path).is_ok_and(|current| current == contents) {
        return Ok(());
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&temporary, contents)
        .with_context(|| format!("cannot write {}", temporary.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(
            &temporary,
            fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }),
        )?;
    }
    #[cfg(not(unix))]
    let _ = executable;
    fs::rename(&temporary, path).with_context(|| format!("cannot write {}", path.display()))
}

fn copy_executable(source: &Path, target: &Path) -> Result<()> {
    let folder = target.parent().context("the copy has a folder")?;
    fs::create_dir_all(folder)?;
    if !copy_is_current(source, target) {
        let temporary = target.with_extension(format!("tmp-{}", std::process::id()));
        fs::copy(source, &temporary)
            .with_context(|| format!("cannot copy Haarness to {}", temporary.display()))?;
        fs::rename(&temporary, target).with_context(|| {
            format!(
                "cannot replace {}; close Chrome, then run setup again",
                target.display()
            )
        })?;
    }
    // Earlier versions' copies; one still running stays until next time.
    if let Ok(entries) = fs::read_dir(folder) {
        for entry in entries.flatten() {
            if entry.path() != target {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    Ok(())
}

fn copy_tree(source: &Path, target: &Path) -> io::Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let destination = target.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &destination)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

/// Copies the extension when the copy is missing or another version, then
/// swaps it in whole.
pub(crate) fn sync_extension(source: Option<&Path>, target: &Path) -> Result<()> {
    let Some(source) = source else {
        return Ok(());
    };
    let wanted = extension_version(source);
    if wanted.is_some() && extension_version(target) == wanted {
        return Ok(());
    }
    let parent = target.parent().context("the extension copy has a folder")?;
    let pid = std::process::id();
    let staging = parent.join(format!(".{EXTENSION_FOLDER}.new-{pid}"));
    let old = parent.join(format!(".{EXTENSION_FOLDER}.old-{pid}"));
    let _ = fs::remove_dir_all(&staging);
    copy_tree(source, &staging)
        .with_context(|| format!("cannot copy Haarness for Chrome to {}", parent.display()))?;
    let _ = fs::remove_dir_all(&old);
    let had = target.exists();
    if had {
        fs::rename(target, &old).with_context(|| format!("cannot replace {}", target.display()))?;
    }
    if let Err(error) = fs::rename(&staging, target) {
        if had {
            let _ = fs::rename(&old, target);
        }
        let _ = fs::remove_dir_all(&staging);
        return Err(error).with_context(|| format!("cannot replace {}", target.display()));
    }
    let _ = fs::remove_dir_all(&old);
    Ok(())
}

const NOT_INCLUDED: &str = "Haarness for Chrome is not included in this installation. Set it up from Haarness Desktop (Settings → Browser → Chrome).";
/// After `remove`, the extension may still be in Chrome; what is gone is
/// Chrome's way to start Haarness.
const REMOVED: &str = "Chrome cannot start Haarness: `haarness browser remove` turned that off. Run `haarness browser setup` to use Chrome again.";

pub(crate) fn setup(context: &Context, options: &SetupOptions) -> Result<Report> {
    let files = ChromeFiles::new(&context.data, context.os);
    fs::create_dir_all(&files.dir)
        .with_context(|| format!("cannot create {}", files.dir.display()))?;
    let manifest_dirs = if options.manifest_dirs.is_empty() {
        context.manifest_dirs.as_slice()
    } else {
        options.manifest_dirs
    };
    let mut warnings = Vec::new();
    if context.isolated && manifest_dirs.is_empty() {
        warnings.push(format!(
            "A separate Haarness data folder is in use ({}), so Chrome will work with that folder's projects only. Run setup without HAARNESS_DATA_DIR or HAARNESS_HOME to use your usual one.",
            context.data.display()
        ));
    }
    let targets = targets(context, options.browsers, manifest_dirs, false);
    if options.if_missing && files.removed().exists() {
        return Ok(report(context, &files, &targets, warnings));
    }
    if !options.if_missing {
        match fs::remove_file(files.removed()) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
    }
    let plan = plan_executable(
        &context.exe,
        context.os,
        context.appimage,
        &files.bin(),
        &context.version,
    )?;
    let wrapper = files.wrapper();
    let rewrite = !options.if_missing
        || wrapper_needs_rewrite(
            wrapper_exe(&wrapper).as_deref(),
            &plan,
            &context.exe,
            &files.bin(),
        );
    if rewrite {
        if let Some(source) = &plan.copy_from {
            copy_executable(source, &plan.run)?;
        }
        let script = manifest::wrapper(context.os, &plan.run, &context.forwarded)?;
        write_if_changed(&wrapper, script.as_bytes(), true)?;
    }
    let contents = serde_json::to_vec_pretty(&manifest::manifest(&wrapper, &context.extra_ids))?;
    for target in &targets {
        if target.place == Place::Unsupported
            || (!rewrite && target.registered(&files, context.registry))
        {
            continue;
        }
        match &target.place {
            Place::File { manifest, .. } => {
                let folder = manifest.parent().context("a manifest has a folder")?;
                fs::create_dir_all(folder)
                    .with_context(|| format!("cannot create {}", folder.display()))?;
                write_if_changed(manifest, &contents, false)?;
            }
            Place::Registry { key, .. } => {
                write_if_changed(&files.windows_manifest(), &contents, false)?;
                if context.registry.get(key).as_deref()
                    != Some(&files.windows_manifest().display().to_string())
                {
                    context.registry.set(key, &files.windows_manifest())?;
                }
            }
            Place::Unsupported => {}
        }
    }
    sync_extension(context.extension_source.as_deref(), &files.extension())?;
    if extension_copy(&files.extension()).is_none() && CHROME_WEB_STORE_URL.is_none() {
        warnings.push(NOT_INCLUDED.into());
    }
    Ok(report(context, &files, &targets, warnings))
}

pub(crate) fn status(context: &Context) -> Report {
    let files = ChromeFiles::new(&context.data, context.os);
    // Setup could not add one either, so say what setup would.
    let warnings = if extension_copy(&files.extension()).is_none()
        && context.extension_source.is_none()
        && CHROME_WEB_STORE_URL.is_none()
    {
        vec![NOT_INCLUDED.to_owned()]
    } else {
        Vec::new()
    };
    report(
        context,
        &files,
        &targets(context, &[], &context.manifest_dirs, false),
        warnings,
    )
}

/// Removes the registrations and wrapper this data folder owns, and marks
/// Chrome as removed so Desktop's launch-time setup leaves it alone.
pub(crate) fn remove(context: &Context, all: bool) -> Result<Report> {
    let files = ChromeFiles::new(&context.data, context.os);
    let wrapper = files.wrapper();
    let mut deleted = Vec::new();
    for target in targets(context, &[], &context.manifest_dirs, true) {
        match &target.place {
            Place::File { manifest, .. } if manifest_points_at(manifest, &wrapper) => {
                fs::remove_file(manifest)
                    .with_context(|| format!("cannot remove {}", manifest.display()))?;
                deleted.push(manifest.display().to_string());
            }
            Place::Registry { key, .. }
                if context.registry.get(key).as_deref()
                    == Some(&files.windows_manifest().display().to_string()) =>
            {
                context.registry.delete(key)?;
                deleted.push(key.clone());
            }
            _ => {}
        }
    }
    for file in [files.windows_manifest(), wrapper] {
        if file.exists() {
            fs::remove_file(&file).with_context(|| format!("cannot remove {}", file.display()))?;
            deleted.push(file.display().to_string());
        }
    }
    let mut folders = vec![files.bin()];
    if all {
        folders.push(files.extension());
    }
    for folder in folders {
        if folder.exists() {
            fs::remove_dir_all(&folder)
                .with_context(|| format!("cannot remove {}", folder.display()))?;
            deleted.push(folder.display().to_string());
        }
    }
    fs::create_dir_all(&files.dir)?;
    fs::write(
        files.removed(),
        "Removed with `haarness browser remove`. Run `haarness browser setup` to use Chrome again.\n",
    )?;
    let mut report = report(
        context,
        &files,
        &targets(context, &[], &context.manifest_dirs, false),
        Vec::new(),
    );
    report.deleted = Some(deleted);
    Ok(report)
}

fn print_json(report: &Report) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(report)?);
    Ok(())
}

fn list(names: &[&str]) -> String {
    names.join(", ")
}

/// The next step people take in Chrome.
fn next_step(report: &Report, os: Os) -> String {
    if let Some(url) = report.web_store_url {
        return format!(
            "Next, add Haarness for Chrome from the Chrome Web Store: {url}\nThen pin Haarness to the toolbar."
        );
    }
    let Some(extension) = &report.extension else {
        return NOT_INCLUDED.into();
    };
    let page = report
        .browsers
        .iter()
        .find(|browser| browser.supported)
        .map_or("chrome://extensions", |browser| {
            browser.extensions_page.as_str()
        });
    let paste = match os {
        Os::Mac => "press ⌘⇧G and paste",
        Os::Linux => "press Ctrl+L and paste",
        Os::Windows => "paste this into the folder box",
    };
    format!(
        "Next, add Haarness for Chrome: open {page}, turn on Developer mode (keep it on), click Load unpacked, {paste}:\n  {}\nThen pin Haarness to the toolbar.",
        extension.path
    )
}

pub(crate) fn setup_command(data: &Path, data_dir: Option<&Path>, args: &SetupArgs) -> Result<()> {
    let context = Context::from_environment(data, data_dir)?;
    let report = setup(
        &context,
        &SetupOptions {
            browsers: &args.browsers,
            manifest_dirs: &args.manifest_dirs,
            if_missing: args.if_missing,
        },
    )?;
    if args.json {
        print_json(&report)?;
    }
    if args.quiet || args.json {
        return Ok(());
    }
    // The next step says this one.
    for warning in report
        .warnings
        .iter()
        .filter(|warning| *warning != NOT_INCLUDED)
    {
        eprintln!("Warning: {warning}");
    }
    if report.removed {
        println!("{REMOVED}");
        return Ok(());
    }
    let registered: Vec<&str> = report
        .browsers
        .iter()
        .filter(|browser| browser.registered)
        .map(|browser| browser.name.as_str())
        .collect();
    if registered.is_empty() {
        println!("No browser could be set up to start Haarness.");
    } else {
        println!("Chrome can now start Haarness ({}).", list(&registered));
    }
    for browser in report.browsers.iter().filter(|browser| !browser.supported) {
        if let Some(note) = &browser.note {
            println!("{}: {note}", browser.name);
        }
    }
    println!("{}", next_step(&report, context.os));
    Ok(())
}

/// Live servers' Chrome connections, from every data root this process can see.
async fn connections(cache: &Path) -> Vec<Value> {
    let env = |key: &str| std::env::var_os(key);
    let mut caches = vec![discovery::canonical(cache)];
    for root in discovery::data_roots(&env, discovery::home_dir().as_deref()) {
        if !caches.contains(&root.cache) {
            caches.push(root.cache);
        }
    }
    let http = discovery::http_client();
    let servers = discovery::Discovery::new(caches, http.clone()).scan().await;
    let asks = servers
        .iter()
        .filter(|server| server.capable)
        .map(|server| {
            let api = relay::Api::new(server, http.clone());
            async move {
                let hosts = api.hosts().await.ok()?;
                Some(json!({
                    "workspace": server.workspace,
                    "serverId": server.id,
                    "hosts": hosts["hosts"].as_array().cloned().unwrap_or_default(),
                }))
            }
        });
    futures::future::join_all(asks)
        .await
        .into_iter()
        .flatten()
        .collect()
}

pub(crate) async fn status_command(data: &Path, cache: &Path, json: bool) -> Result<()> {
    let context = Context::from_environment(data, None)?;
    let mut report = status(&context);
    let connections = connections(cache).await;
    if json {
        report.connections = Some(connections);
        return print_json(&report);
    }
    if report.removed {
        println!("{REMOVED}");
        return Ok(());
    }
    let registered: Vec<&str> = report
        .browsers
        .iter()
        .filter(|browser| browser.registered)
        .map(|browser| browser.name.as_str())
        .collect();
    if registered.is_empty() {
        println!("Chrome cannot start Haarness yet. Run `haarness browser setup`.");
    } else {
        println!("Chrome can start Haarness ({}).", list(&registered));
    }
    for browser in report.browsers.iter().filter(|browser| !browser.supported) {
        if let Some(note) = &browser.note {
            println!("{}: {note}", browser.name);
        }
    }
    match &report.extension {
        Some(extension) => println!(
            "Haarness for Chrome {}: {}",
            extension.version, extension.path
        ),
        None if report
            .warnings
            .iter()
            .any(|warning| warning == NOT_INCLUDED) =>
        {
            println!("{NOT_INCLUDED}");
        }
        None => {
            println!("Haarness for Chrome is not copied here yet. Run `haarness browser setup`.")
        }
    }
    println!("{}", connected_summary(&connections));
    Ok(())
}

/// `status`'s line on the browsers connected to live servers, saying which
/// the person paused or stopped (agents cannot use those).
fn connected_summary(connections: &[Value]) -> String {
    let mut connected = Vec::new();
    for connection in connections {
        let project = connection["workspace"]
            .as_str()
            .and_then(|workspace| Path::new(workspace).file_name())
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        for host in connection["hosts"].as_array().into_iter().flatten() {
            let mut name = host["name"].as_str().unwrap_or("Chrome").to_owned();
            if let Some(label) = host["label"].as_str() {
                let _ = write!(name, " ({label})");
            }
            let _ = write!(name, " · {project}");
            if host["stopped"] == true {
                name.push_str(" · stopped in Chrome");
            } else if host["paused"] == true {
                name.push_str(" · paused in Chrome");
            }
            connected.push(name);
        }
    }
    if connected.is_empty() {
        "No Chrome is connected to a project right now.".into()
    } else {
        format!("Connected: {}", connected.join(", "))
    }
}

pub(crate) fn remove_command(data: &Path, all: bool, json: bool) -> Result<()> {
    let context = Context::from_environment(data, None)?;
    let report = remove(&context, all)?;
    if json {
        return print_json(&report);
    }
    let deleted = report.deleted.as_deref().unwrap_or_default();
    if deleted.is_empty() {
        println!("Chrome was not set up to start Haarness here.");
    } else {
        println!("Chrome can no longer start Haarness. Removed:");
        for path in deleted {
            println!("  {path}");
        }
    }
    if let Some(extension) = report.extension.as_ref().filter(|_| !all) {
        println!(
            "Haarness for Chrome is still at {}; remove it in Chrome, or run `haarness browser remove --all`.",
            extension.path
        );
    }
    Ok(())
}

/// `haarness doctor`'s line: local files only, never an error.
pub(crate) fn doctor_summary(data: &Path) -> String {
    let files = ChromeFiles::new(data, Os::current());
    if files.removed().exists() {
        return "chrome: removed (run `haarness browser setup` to use Chrome again)".into();
    }
    let Ok(context) = Context::from_environment(data, None) else {
        return "chrome: unknown".into();
    };
    let report = status(&context);
    let registered: Vec<&str> = report
        .browsers
        .iter()
        .filter(|browser| browser.registered)
        .map(|browser| browser.name.as_str())
        .collect();
    if registered.is_empty() {
        return "chrome: not set up (run `haarness browser setup`)".into();
    }
    let extension = report.extension.map_or_else(String::new, |extension| {
        format!("; Haarness for Chrome {}", extension.version)
    });
    format!(
        "chrome: can start Haarness ({}){extension}",
        list(&registered)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::discovery::tests::temp;
    use std::{cell::RefCell, collections::HashMap};

    #[derive(Default)]
    struct FakeRegistry(RefCell<HashMap<String, String>>);

    impl Registry for FakeRegistry {
        fn get(&self, key: &str) -> Option<String> {
            self.0.borrow().get(key).cloned()
        }
        fn set(&self, key: &str, manifest: &Path) -> Result<()> {
            self.0
                .borrow_mut()
                .insert(key.into(), manifest.display().to_string());
            Ok(())
        }
        fn delete(&self, key: &str) -> Result<()> {
            self.0.borrow_mut().remove(key);
            Ok(())
        }
    }

    struct Fixture {
        root: PathBuf,
        registry: FakeRegistry,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = temp(name);
            fs::create_dir_all(root.join("home")).unwrap();
            fs::create_dir_all(root.join("data")).unwrap();
            fs::create_dir_all(root.join("install/bin")).unwrap();
            fs::write(root.join("install/bin/haarness"), b"#!/bin/sh\n").unwrap();
            let extension = root.join("install/browser-extension/chrome");
            fs::create_dir_all(extension.join("icons")).unwrap();
            fs::write(
                extension.join("manifest.json"),
                br#"{"version":"0.1.1.1","version_name":"0.1.1-preview.1"}"#,
            )
            .unwrap();
            fs::write(extension.join("icons/16.png"), b"png").unwrap();
            Self {
                root,
                registry: FakeRegistry::default(),
            }
        }

        fn context(&self, os: Os) -> Context<'_> {
            let exe = self.root.join("install/bin/haarness");
            Context {
                os,
                home: self.root.join("home"),
                local_app_data: Some(self.root.join("home/AppData/Local")),
                data: self.root.join("data"),
                extension_source: extension_source(None, &exe),
                exe,
                version: "0.1.1".into(),
                appimage: false,
                forwarded: vec![("HAARNESS_DATA_DIR", self.root.display().to_string())],
                isolated: false,
                extra_ids: Vec::new(),
                manifest_dirs: Vec::new(),
                registry: &self.registry,
            }
        }

        fn home(&self, path: &str) -> PathBuf {
            self.root.join("home").join(path)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// `root` joined one part at a time, as the code under test joins paths.
    fn under(root: &Path, parts: &[&str]) -> PathBuf {
        parts
            .iter()
            .fold(root.to_path_buf(), |path, part| path.join(part))
    }

    fn options(if_missing: bool) -> SetupOptions<'static> {
        SetupOptions {
            browsers: &[],
            manifest_dirs: &[],
            if_missing,
        }
    }

    #[test]
    fn executables_run_in_place_unless_their_path_does_not_last() {
        let bin = Path::new("/data/browser/chrome/bin");
        let plan =
            |exe: &str, os, appimage| plan_executable(Path::new(exe), os, appimage, bin, "0.1.1");
        assert_eq!(
            plan(
                "/Applications/Haarness.app/Contents/Resources/bin/haarness",
                Os::Mac,
                false
            )
            .unwrap(),
            ExePlan {
                run: PathBuf::from("/Applications/Haarness.app/Contents/Resources/bin/haarness"),
                copy_from: None
            }
        );
        assert_eq!(
            plan("/w/target/debug/haarness", Os::Linux, false)
                .unwrap()
                .copy_from,
            None
        );
        let copied = plan("/tmp/.mount_HaarnXYZ/usr/bin/haarness", Os::Linux, false).unwrap();
        assert_eq!(copied.run, bin.join("haarness"));
        assert_eq!(
            copied.copy_from.as_deref(),
            Some(Path::new("/tmp/.mount_HaarnXYZ/usr/bin/haarness"))
        );
        assert_eq!(
            plan("/opt/Haarness/haarness", Os::Linux, true).unwrap().run,
            bin.join("haarness")
        );
        let windows = plan(
            "C:\\Program Files\\Haarness\\resources\\bin\\haarness.exe",
            Os::Windows,
            false,
        )
        .unwrap();
        assert_eq!(windows.run, bin.join("haarness-0.1.1.exe"));
        for moving in [
            "/private/var/folders/x/AppTranslocation/ABC/d/Haarness.app/Contents/Resources/bin/haarness",
            "/Volumes/Haarness/Haarness.app/Contents/Resources/bin/haarness",
        ] {
            let error = plan(moving, Os::Mac, false).unwrap_err().to_string();
            assert!(
                error.contains("Move Haarness to Applications, then run setup again"),
                "{error}"
            );
        }
    }

    #[test]
    fn if_missing_rewrites_only_a_wrapper_that_cannot_work() {
        let root = temp("needs-rewrite");
        let bin = root.join("bin");
        let installed = root.join("installed/haarness");
        let dev = root.join("w/target/debug/haarness");
        for exe in [&installed, &dev] {
            fs::create_dir_all(exe.parent().unwrap()).unwrap();
            fs::write(exe, b"x").unwrap();
        }
        let in_place = |exe: &Path| ExePlan {
            run: exe.to_path_buf(),
            copy_from: None,
        };
        // No wrapper, or its executable is gone.
        assert!(wrapper_needs_rewrite(
            None,
            &in_place(&installed),
            &installed,
            &bin
        ));
        assert!(wrapper_needs_rewrite(
            Some(&root.join("gone")),
            &in_place(&installed),
            &installed,
            &bin
        ));
        // A working wrapper is left alone, even for another installed build.
        assert!(!wrapper_needs_rewrite(
            Some(&installed),
            &in_place(&dev),
            &dev,
            &bin
        ));
        assert!(!wrapper_needs_rewrite(
            Some(&dev),
            &in_place(&dev),
            &dev,
            &bin
        ));
        // A development build gives way to an installed one.
        assert!(wrapper_needs_rewrite(
            Some(&dev),
            &in_place(&installed),
            &installed,
            &bin
        ));
        // A copy of an unstable executable is refreshed when it differs.
        fs::create_dir_all(&bin).unwrap();
        let copy = bin.join("haarness-0.1.1.exe");
        let plan = ExePlan {
            run: copy.clone(),
            copy_from: Some(installed.clone()),
        };
        fs::write(&copy, b"x").unwrap();
        assert!(!wrapper_needs_rewrite(Some(&copy), &plan, &installed, &bin));
        fs::write(&installed, b"newer build").unwrap();
        assert!(wrapper_needs_rewrite(Some(&copy), &plan, &installed, &bin));
        let older = bin.join("haarness-0.1.0.exe");
        fs::write(&older, b"x").unwrap();
        assert!(wrapper_needs_rewrite(Some(&older), &plan, &installed, &bin));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn setup_registers_chrome_and_detected_browsers_and_copies_the_extension() {
        let fixture = Fixture::new("setup-mac");
        fs::create_dir_all(fixture.home("Library/Application Support/BraveSoftware/Brave-Browser"))
            .unwrap();
        let context = fixture.context(Os::Mac);
        let report = setup(&context, &options(false)).unwrap();
        let json = serde_json::to_value(&report).unwrap();
        let chrome_dir = under(&fixture.root, &["data", "browser", "chrome"]);
        let wrapper = chrome_dir.join("haarness-browser-host");
        let extension = chrome_dir.join("Haarness for Chrome");
        assert_eq!(json["hostName"], "ai.haarness.browser");
        assert_eq!(json["extensionId"], "accdjhmcgljbadpeanajllmcembhkomn");
        assert_eq!(json["webStoreUrl"], Value::Null);
        assert_eq!(json["executable"], context.exe.display().to_string());
        assert_eq!(json["wrapper"], wrapper.display().to_string());
        assert_eq!(json["removed"], false);
        assert_eq!(json["warnings"], json!([]));
        assert_eq!(
            json["extension"],
            json!({"path": extension.display().to_string(), "version": "0.1.1-preview.1"})
        );
        assert!(json.get("connections").is_none() && json.get("deleted").is_none());
        let keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            keys,
            [
                "hostName",
                "extensionId",
                "webStoreUrl",
                "executable",
                "wrapper",
                "removed",
                "warnings",
                "browsers",
                "extension"
            ]
        );
        // Joined as `manifest::place` joins them, so the strings match on Windows too.
        let host_manifest = |folder: &str| {
            under(
                &fixture.root.join("home"),
                &[
                    "Library/Application Support",
                    folder,
                    "NativeMessagingHosts",
                    "ai.haarness.browser.json",
                ],
            )
        };
        let chrome_manifest = host_manifest("Google/Chrome");
        assert_eq!(
            json["browsers"],
            json!([
                {"browser":"chrome","name":"Google Chrome","extensionsPage":"chrome://extensions",
                 "manifest": chrome_manifest.display().to_string(),"detected":false,"registered":true,"supported":true,"note":null},
                {"browser":"brave","name":"Brave","extensionsPage":"brave://extensions",
                 "manifest": host_manifest("BraveSoftware/Brave-Browser").display().to_string(),
                 "detected":true,"registered":true,"supported":true,"note":null}
            ])
        );
        let manifest: Value = serde_json::from_slice(&fs::read(&chrome_manifest).unwrap()).unwrap();
        assert_eq!(manifest, manifest::manifest(&wrapper, &[]));
        let script = fs::read_to_string(&wrapper).unwrap();
        assert_eq!(
            script,
            manifest::shell_wrapper(
                &context.exe,
                &[("HAARNESS_DATA_DIR", fixture.root.display().to_string())]
            )
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&wrapper).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
        assert_eq!(fs::read(extension.join("icons/16.png")).unwrap(), b"png");
        assert_eq!(status(&context).browsers.len(), 2);

        // A new extension version replaces the copy; an equal one is left alone.
        let source = fixture.root.join("install/browser-extension/chrome");
        fs::write(extension.join("stale.js"), b"old").unwrap();
        setup(&context, &options(true)).unwrap();
        assert!(extension.join("stale.js").exists());
        fs::write(
            source.join("manifest.json"),
            br#"{"version":"0.1.2.1000","version_name":"0.1.2"}"#,
        )
        .unwrap();
        let report = setup(&context, &options(true)).unwrap();
        assert_eq!(report.extension.unwrap().version, "0.1.2");
        assert!(!extension.join("stale.js").exists());
    }

    #[test]
    fn a_manifest_dir_gets_the_only_manifest_and_an_isolated_root_warns_without_one() {
        let fixture = Fixture::new("setup-manifest-dir");
        let mut context = fixture.context(Os::Mac);
        context.isolated = true;
        let profile = fixture.root.join("profile/NativeMessagingHosts");
        let dirs = [profile.clone()];
        let report = setup(
            &context,
            &SetupOptions {
                browsers: &[],
                manifest_dirs: &dirs,
                if_missing: false,
            },
        )
        .unwrap();
        assert!(report.warnings.is_empty());
        assert_eq!(report.browsers.len(), 1);
        assert_eq!(report.browsers[0].browser, "custom");
        assert!(report.browsers[0].registered);
        assert!(profile.join("ai.haarness.browser.json").is_file());
        assert!(
            !fixture.home("Library").exists(),
            "nothing written for the real browsers"
        );
        let report = setup(&context, &options(false)).unwrap();
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].contains("separate Haarness data folder"));
    }

    #[test]
    fn the_test_manifest_folders_stand_in_for_every_browser_in_setup_status_and_remove() {
        // As the TUI's /chrome and Desktop's setup dialog run it: no
        // --manifest-dir, only HAARNESS_BROWSER_MANIFEST_DIR.
        let fixture = Fixture::new("setup-env-manifest-dir");
        fs::create_dir_all(fixture.home("Library/Application Support/Google/Chrome")).unwrap();
        let mut context = fixture.context(Os::Mac);
        context.isolated = true;
        let profiles = [
            fixture.root.join("a/NativeMessagingHosts"),
            fixture.root.join("b/NativeMessagingHosts"),
        ];
        context.manifest_dirs = profiles.to_vec();
        let report = setup(&context, &options(false)).unwrap();
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(report.browsers.len(), 2);
        assert!(report.browsers.iter().all(|browser| browser.registered));
        let real = fixture.home("Library/Application Support/Google/Chrome/NativeMessagingHosts");
        assert!(!real.exists(), "nothing written for the real browsers");
        let report = status(&context);
        assert!(
            report
                .browsers
                .iter()
                .all(|browser| browser.browser == "custom" && browser.registered)
        );
        let report = remove(&context, false).unwrap();
        for profile in &profiles {
            assert!(!profile.join("ai.haarness.browser.json").exists());
        }
        assert_eq!(
            report.deleted.unwrap().len(),
            3,
            "two manifests and the wrapper"
        );
        assert!(report.browsers.iter().all(|browser| !browser.registered));
        assert!(!real.exists());
    }

    #[test]
    fn remove_deletes_what_setup_made_and_if_missing_respects_it() {
        let fixture = Fixture::new("remove");
        let context = fixture.context(Os::Linux);
        fs::create_dir_all(fixture.home(".config/chromium")).unwrap();
        setup(&context, &options(false)).unwrap();
        // Another data folder's manifest for a browser is not this one's to remove.
        let edge =
            fixture.home(".config/microsoft-edge/NativeMessagingHosts/ai.haarness.browser.json");
        fs::create_dir_all(edge.parent().unwrap()).unwrap();
        fs::write(
            &edge,
            serde_json::to_vec(&manifest::manifest(Path::new("/elsewhere/host"), &[])).unwrap(),
        )
        .unwrap();

        let report = remove(&context, false).unwrap();
        let deleted = report.deleted.clone().unwrap();
        let removed = |tail: &str| deleted.iter().any(|path| Path::new(path).ends_with(tail));
        assert!(removed(
            "google-chrome/NativeMessagingHosts/ai.haarness.browser.json"
        ));
        assert!(removed(
            "chromium/NativeMessagingHosts/ai.haarness.browser.json"
        ));
        assert!(
            deleted
                .iter()
                .any(|path| path.ends_with("haarness-browser-host"))
        );
        assert!(edge.exists());
        assert!(report.removed);
        assert!(report.browsers.iter().all(|browser| !browser.registered));
        assert!(
            report.extension.is_some(),
            "the extension copy stays without --all"
        );

        // Desktop's launch-time setup leaves a removal alone.
        let report = setup(&context, &options(true)).unwrap();
        assert!(report.removed);
        assert!(!Path::new(&report.wrapper).exists());
        // An explicit setup brings it back.
        let report = setup(&context, &options(false)).unwrap();
        assert!(!report.removed);
        assert!(
            report
                .browsers
                .iter()
                .filter(|browser| browser.supported)
                .all(|browser| browser.registered)
        );
        let report = remove(&context, true).unwrap();
        assert!(report.extension.is_none());
    }

    #[test]
    fn if_missing_repairs_registrations_but_keeps_a_working_wrapper() {
        let fixture = Fixture::new("if-missing");
        let context = fixture.context(Os::Linux);
        // A wrapper for another installed Haarness stays.
        let other = fixture.root.join("other/haarness");
        fs::create_dir_all(other.parent().unwrap()).unwrap();
        fs::write(&other, b"x").unwrap();
        let files = ChromeFiles::new(&context.data, Os::Linux);
        fs::create_dir_all(&files.dir).unwrap();
        fs::write(files.wrapper(), manifest::shell_wrapper(&other, &[])).unwrap();
        let report = setup(&context, &options(true)).unwrap();
        assert_eq!(report.executable, other.display().to_string());
        assert!(
            report.browsers[0].registered,
            "the missing manifest was written"
        );
        // Once its executable is gone, it points at this one.
        fs::remove_file(&other).unwrap();
        let report = setup(&context, &options(true)).unwrap();
        assert_eq!(report.executable, context.exe.display().to_string());
        assert!(report.browsers[0].registered);
    }

    #[test]
    fn linux_lists_sandboxed_browsers_as_unsupported() {
        let fixture = Fixture::new("sandboxed");
        fs::create_dir_all(fixture.home(".var/app/com.google.Chrome")).unwrap();
        fs::create_dir_all(fixture.home("snap/chromium")).unwrap();
        let context = fixture.context(Os::Linux);
        let report = setup(&context, &options(false)).unwrap();
        let sandboxed: Vec<_> = report
            .browsers
            .iter()
            .filter(|browser| !browser.supported)
            .collect();
        assert_eq!(sandboxed.len(), 2);
        assert_eq!(sandboxed[0].name, "Google Chrome (Flatpak)");
        assert_eq!(sandboxed[1].name, "Chromium (Snap)");
        assert!(sandboxed.iter().all(|browser| !browser.registered
            && browser.manifest.is_none()
            && browser.note.is_some()));
    }

    #[test]
    fn windows_copies_the_executable_and_registers_through_the_registry() {
        let fixture = Fixture::new("setup-windows");
        // Joined as `place` joins it, so the test runs on every platform.
        fs::create_dir_all(
            fixture
                .home("AppData/Local")
                .join("Microsoft\\Edge")
                .join("User Data"),
        )
        .unwrap();
        let context = fixture.context(Os::Windows);
        let report = setup(&context, &options(false)).unwrap();
        let chrome_dir = under(&fixture.root, &["data", "browser", "chrome"]);
        let copy = chrome_dir.join("bin").join("haarness-0.1.1.exe");
        let manifest_file = chrome_dir.join("ai.haarness.browser.json");
        assert_eq!(report.executable, copy.display().to_string());
        assert_eq!(fs::read(&copy).unwrap(), fs::read(&context.exe).unwrap());
        assert_eq!(
            report.wrapper,
            chrome_dir
                .join("haarness-browser-host.bat")
                .display()
                .to_string()
        );
        let script = fs::read_to_string(&report.wrapper).unwrap();
        assert!(script.starts_with("@echo off\r\n"), "{script}");
        assert_eq!(manifest::wrapper_target(&script), Some(copy.clone()));
        let names: Vec<_> = report
            .browsers
            .iter()
            .map(|browser| (browser.browser.as_str(), browser.registered))
            .collect();
        assert_eq!(names, [("chrome", true), ("edge", true)]);
        assert_eq!(
            fixture
                .registry
                .get("HKCU\\Software\\Microsoft\\Edge\\NativeMessagingHosts\\ai.haarness.browser"),
            Some(manifest_file.display().to_string())
        );
        let manifest: Value = serde_json::from_slice(&fs::read(&manifest_file).unwrap()).unwrap();
        assert_eq!(manifest["path"], report.wrapper);

        let report = remove(&context, false).unwrap();
        assert!(fixture.registry.0.borrow().is_empty());
        assert!(!copy.exists() && !manifest_file.exists());
        assert!(
            report
                .deleted
                .unwrap()
                .iter()
                .any(|key| key.starts_with("HKCU\\Software\\Google\\Chrome"))
        );
    }

    #[test]
    fn status_says_when_this_installation_has_no_extension_to_add() {
        let fixture = Fixture::new("status-no-extension");
        let mut context = fixture.context(Os::Mac);
        assert!(status(&context).warnings.is_empty(), "setup can copy it");
        context.extension_source = None;
        assert_eq!(status(&context).warnings, [NOT_INCLUDED]);
        // A copy Desktop left in the data folder is enough.
        let files = ChromeFiles::new(&context.data, context.os);
        sync_extension(
            Some(&fixture.root.join("install/browser-extension/chrome")),
            &files.extension(),
        )
        .unwrap();
        assert!(status(&context).warnings.is_empty());
    }

    #[test]
    fn status_says_which_connected_browsers_agents_cannot_use() {
        let host = |label: &str, paused: bool, stopped: bool| json!({"name":"Google Chrome","label":label,"paused":paused,"stopped":stopped});
        let connections = [json!({
            "workspace": "/work/shop",
            "hosts": [host("Work", false, false), host("Home", true, false), host("Test", true, true)],
        })];
        assert_eq!(
            connected_summary(&connections),
            "Connected: Google Chrome (Work) · shop, Google Chrome (Home) · shop · paused in Chrome, Google Chrome (Test) · shop · stopped in Chrome"
        );
        assert_eq!(
            connected_summary(&[]),
            "No Chrome is connected to a project right now."
        );
        // The extension outlives `remove`; only Chrome's way in is gone.
        assert!(REMOVED.starts_with("Chrome cannot start Haarness"));
    }

    #[test]
    fn the_next_step_names_the_extension_folder_and_the_browsers_page() {
        let fixture = Fixture::new("next-step");
        let context = fixture.context(Os::Mac);
        let report = setup(&context, &options(false)).unwrap();
        let step = next_step(&report, Os::Mac);
        assert!(step.starts_with("Next, add Haarness for Chrome: open chrome://extensions, turn on Developer mode (keep it on), click Load unpacked, press ⌘⇧G and paste:\n  "));
        assert!(step.contains("Haarness for Chrome\nThen pin Haarness to the toolbar."));
        assert!(next_step(&report, Os::Linux).contains("press Ctrl+L and paste"));
    }
}
