//! Client-side file locations and persistence (prompt history, later theme
//! state). Mirrors the CLI's path resolution without linking it.

use std::path::PathBuf;

use crate::input::composer::HISTORY_CAP;

/// How the root page interacts with the terminal's configured background.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackgroundMode {
    /// Preserve the terminal background when exact theme colors are unavailable.
    #[default]
    Auto,
    /// Always paint the theme's page background.
    Theme,
    /// Always let the terminal's configured background show through.
    Terminal,
}

impl BackgroundMode {
    /// A theme explicitly picked or previewed by the user owns its background.
    /// The terminal override still wins when explicitly configured.
    pub const fn for_theme_choice(self, explicit: bool) -> Self {
        if explicit && matches!(self, Self::Auto) {
            Self::Theme
        } else {
            self
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(untagged)]
pub enum StatusLineCommand {
    Shell(String),
    Argv(Vec<String>),
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct StatusLineConfig {
    pub command: StatusLineCommand,
    pub refresh_ms: u64,
    pub timeout_ms: u64,
}

impl Default for StatusLineConfig {
    fn default() -> Self {
        Self {
            command: StatusLineCommand::Argv(Vec::new()),
            refresh_ms: 2_000,
            timeout_ms: 5_000,
        }
    }
}

/// The folder that holds config.toml for every Haarness client, resolved as
/// the CLI's storage layout does: `HAARNESS_DATA_DIR`, then `HAARNESS_HOME`,
/// then `$XDG_CONFIG_HOME/haarness` or `~/.config/haarness` on every OS.
fn resolve_config_dir(
    env: impl Fn(&str) -> Option<std::ffi::OsString>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(root) = env("HAARNESS_DATA_DIR") {
        return Some(PathBuf::from(root).join("config"));
    }
    if let Some(root) = env("HAARNESS_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Some(root.join("config"));
    }
    env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| home.map(|home| home.join(".config")))
        .map(|base| base.join("haarness"))
}

/// Where earlier versions kept the TUI's files: the platform folder, which
/// on macOS is `~/Library/Application Support/dev.haarness.haarness` rather
/// than the folder everything else reads. `None` where the two agree, or when
/// an explicit root makes the install self-contained.
fn legacy_config_dir() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    if std::env::var_os("HAARNESS_DATA_DIR").is_some()
        || std::env::var_os("HAARNESS_HOME").is_some_and(|value| !value.is_empty())
    {
        return None;
    }
    let legacy = directories::ProjectDirs::from("dev", "haarness", "haarness")?
        .config_dir()
        .to_path_buf();
    (Some(&legacy) != current_config_dir().as_ref()).then_some(legacy)
}

/// The TUI's own files, which follow it to the shared folder.
const CLIENT_FILES: [&str; 7] = [
    "tui-state.toml",
    "tui-history.jsonl",
    "tui-history-v2.json",
    "tui-stash.json",
    "tui-servers.json",
    "tui-server-drafts.json",
    "themes",
];

/// Copy the TUI's files from the old folder into the shared one, once, without
/// overwriting anything already there. The old copies stay where they are.
fn adopt_legacy_files(target: &std::path::Path, legacy: &std::path::Path) {
    for name in CLIENT_FILES {
        let from = legacy.join(name);
        let to = target.join(name);
        if to.exists() || !from.exists() {
            continue;
        }
        let _ = std::fs::create_dir_all(target);
        if from.is_dir() {
            let _ = std::fs::create_dir_all(&to);
            if let Ok(files) = std::fs::read_dir(&from) {
                for file in files.flatten().filter(|file| file.path().is_file()) {
                    let _ = std::fs::copy(file.path(), to.join(file.file_name()));
                }
            }
        } else {
            let _ = std::fs::copy(&from, &to);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unnecessary_wraps)] // same signature as outside tests, where it may be unknown
fn current_config_dir() -> Option<PathBuf> {
    Some(test_config_dir().to_path_buf())
}

#[cfg(not(test))]
fn current_config_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()));
    resolve_config_dir(|key| std::env::var_os(key), home)
}

/// Unit tests keep the TUI's files in a folder of their own, so a test run
/// never reads or rewrites the developer's state, history or drafts. Each
/// run starts empty, and folders runs left more than six hours ago go.
#[cfg(test)]
fn test_config_dir() -> &'static std::path::Path {
    static DIRECTORY: std::sync::LazyLock<PathBuf> = std::sync::LazyLock::new(|| {
        let base = std::env::temp_dir().join("haarness-tui-unit-tests");
        let stale = std::time::SystemTime::now() - std::time::Duration::from_secs(6 * 60 * 60);
        for entry in std::fs::read_dir(&base).into_iter().flatten().flatten() {
            if entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .is_ok_and(|modified| modified < stale)
            {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
        base.join(format!("{}-{}", std::process::id(), uuid::Uuid::now_v7()))
            .join("config")
    });
    &DIRECTORY
}

fn config_dir() -> Option<PathBuf> {
    static ADOPTED: std::sync::Once = std::sync::Once::new();
    let directory = current_config_dir()?;
    ADOPTED.call_once(|| {
        if let Some(legacy) = legacy_config_dir() {
            adopt_legacy_files(&directory, &legacy);
        }
    });
    Some(directory)
}

/// The prompt history, as `save_scoped_history` keeps it.
pub(crate) const HISTORY_FILE: &str = "tui-history-v2.json";

/// The text of one of the TUI's own files, or nothing when it is missing.
pub(crate) fn client_file_text(name: &str) -> String {
    config_dir()
        .and_then(|directory| std::fs::read_to_string(directory.join(name)).ok())
        .unwrap_or_default()
}

fn history_path() -> Option<PathBuf> {
    config_dir().map(|directory| directory.join("tui-history.jsonl"))
}

/// Prompt history, oldest first. Missing or unreadable files are an empty
/// history, never an error.
pub fn load_history() -> Vec<String> {
    let Some(path) = history_path() else {
        return Vec::new();
    };
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<String>(line).ok())
        .collect()
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct HistoryEntry {
    scope: String,
    thread: Option<String>,
    text: String,
}
fn history_order(entries: &[HistoryEntry], scope: &str, thread: &str) -> Vec<String> {
    let mut newest = Vec::new();
    for current in [true, false] {
        for entry in entries.iter().rev().filter(|entry| {
            entry.scope == scope && (entry.thread.as_deref() == Some(thread)) == current
        }) {
            if !newest.contains(&entry.text) {
                newest.push(entry.text.clone());
            }
        }
    }
    newest.reverse(); // Composer recalls from the end.
    newest
}
fn scoped_entries(path: &std::path::Path) -> Vec<HistoryEntry> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|body| serde_json::from_str(&body).ok())
        .unwrap_or_default()
}
pub(crate) fn load_scoped_history(scope: &str, thread: &str) -> Vec<String> {
    let Some(root) = config_dir() else {
        return Vec::new();
    };
    let path = root.join(HISTORY_FILE);
    // Do not copy the old global file into unrelated projects. Keep it intact;
    // legacy prompts are adopted by the first workspace that records a prompt.
    history_order(&scoped_entries(&path), scope, thread)
}
pub(crate) fn save_scoped_history(scope: &str, thread: &str, text: &str) -> std::io::Result<()> {
    use fs2::FileExt;
    if text.trim().is_empty() {
        return Ok(());
    }
    let Some(root) = config_dir() else {
        return Ok(());
    };
    std::fs::create_dir_all(&root)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("tui-history-v2.lock"))?;
    lock.lock_exclusive()?;
    let path = root.join(HISTORY_FILE);
    let mut entries: Vec<HistoryEntry> = match std::fs::read(&path) {
        Ok(body) => serde_json::from_slice(&body)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    if !path.exists() {
        entries.extend(load_history().into_iter().map(|text| HistoryEntry {
            scope: scope.into(),
            thread: None,
            text,
        }));
    }
    entries.retain(|entry| {
        !(entry.scope == scope && entry.thread.as_deref() == Some(thread) && entry.text == text)
    });
    entries.push(HistoryEntry {
        scope: scope.into(),
        thread: Some(thread.into()),
        text: text.into(),
    });
    let mut count = 0;
    entries.reverse();
    entries.retain(|entry| {
        if entry.scope != scope {
            return true;
        }
        count += 1;
        count <= HISTORY_CAP
    });
    entries.truncate(5000);
    entries.reverse();
    let temporary = root.join(format!("tui-history-{}.tmp", uuid::Uuid::now_v7()));
    let body = serde_json::to_vec(&entries)?;
    std::fs::write(&temporary, body)?;
    std::fs::rename(temporary, path)
}

#[cfg(test)]
mod history_tests {
    #[test]
    fn recall_prioritizes_the_conversation_and_never_crosses_workspaces() {
        let entry = |scope: &str, thread: &str, text: &str| super::HistoryEntry {
            scope: scope.into(),
            thread: Some(thread.into()),
            text: text.into(),
        };
        let entries = [
            entry("a", "first", "shared"),
            entry("a", "first", "mine"),
            entry("b", "first", "private"),
            entry("a", "other", "shared"),
            entry("a", "other", "latest"),
        ];
        assert_eq!(
            super::history_order(&entries, "a", "first"),
            ["latest", "shared", "mine"]
        );
        assert_eq!(super::history_order(&entries, "b", "first"), ["private"]);
    }
}

/// The system configuration folder (`HAARNESS_SYSTEM_CONFIG_DIR`) this TUI
/// started with; never the machine's own in this crate's tests.
pub(crate) fn system_config_dir() -> Option<PathBuf> {
    if cfg!(test) {
        None
    } else {
        haarness_protocol::system_config_dir()
    }
}

/// A theme file that could not be read as one: its file name and the first
/// line of why (the whole reason goes to the log).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadTheme {
    pub file: String,
    pub reason: String,
}

/// Themes discovered from `<config>/themes/*.toml`, then from the base
/// configuration folder's `themes` (`HAARNESS_SYSTEM_CONFIG_DIR`), appended
/// after the built-ins; the first theme of a name wins, so a global theme
/// replaces the system folder's. Unparseable files are skipped and returned,
/// never printed: the TUI may be drawing on the terminal.
pub fn load_theme_entries() -> (Vec<crate::theme::ThemeEntry>, Vec<UnreadTheme>) {
    let mut entries = crate::theme::builtin_entries();
    let mut names: std::collections::HashSet<String> =
        entries.iter().map(|entry| entry.name.clone()).collect();
    let mut unread = Vec::new();
    let directories = config_dir()
        .into_iter()
        .chain(system_config_dir())
        .map(|directory| directory.join("themes"));
    for directory in directories {
        add_theme_entries(&mut entries, &mut names, &mut unread, &directory);
    }
    (entries, unread)
}

fn add_theme_entries(
    entries: &mut Vec<crate::theme::ThemeEntry>,
    names: &mut std::collections::HashSet<String>,
    unread: &mut Vec<UnreadTheme>,
    directory: &std::path::Path,
) {
    let Ok(files) = std::fs::read_dir(directory) else {
        return;
    };
    let mut files: Vec<_> = files.flatten().collect();
    files.sort_by_key(std::fs::DirEntry::path);
    for file in files {
        let path = file.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("toml") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("theme")
            .to_owned();
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        match crate::theme::parse_theme_file(&stem, &content) {
            Ok(entry) => {
                if names.insert(entry.name.clone()) {
                    entries.push(entry);
                }
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), "theme not loaded: {error}");
                unread.push(UnreadTheme {
                    file: file.file_name().to_string_lossy().into_owned(),
                    reason: error.lines().next().unwrap_or_default().trim().to_owned(),
                });
            }
        }
    }
}

/// Session-scoped TUI state persisted across restarts (`tui-state.toml`):
/// theme choice, last open thread, and its scroll position — so reopening
/// haarness lands exactly where the user left off.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct TuiState {
    pub keybindings: std::collections::BTreeMap<String, String>,
    pub model_favorites: Vec<String>,
    pub model_recents: Vec<String>,
    pub theme: Option<String>,
    pub variant: Option<String>,
    pub language: Option<String>,
    /// `compact`, `show` or `hide`; absent until Ctrl+T changes it.
    pub thinking: Option<String>,
    /// Whether the sidebar docks beside the conversation; absent until
    /// Ctrl+B changes it on a terminal wide enough to dock it.
    pub sidebar: Option<bool>,
    pub last_thread: Option<String>,
    /// Legacy single-thread scroll value; read as a fallback for two release
    /// cycles after per-thread view state shipped.
    pub viewport_offset: Option<usize>,
    pub thread_views: std::collections::BTreeMap<String, ThreadViewState>,
    pub diff: DiffViewState,
    /// When the release service last answered (Unix seconds), and the newest
    /// version it named; see `updates.rs`.
    pub update_checked_at: Option<i64>,
    pub latest_release: Option<String>,
}

/// Review-viewer layout choices. They are a reading preference, not session
/// data, so they outlive both the viewer and the thread it was opened from.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DiffViewState {
    pub context_step: usize,
    /// `split`, `unified`, or absent for "whatever fits".
    pub layout: Option<String>,
    pub tree_visible: bool,
    pub single_file: bool,
}

impl Default for DiffViewState {
    fn default() -> Self {
        let defaults = crate::diffview::DiffPreferences::default();
        Self {
            context_step: defaults.context_step,
            layout: None,
            tree_visible: defaults.tree_visible,
            single_file: defaults.single_file,
        }
    }
}

pub fn load_diff_preferences() -> crate::diffview::DiffPreferences {
    let stored = load_state().diff;
    crate::diffview::DiffPreferences {
        context_step: stored
            .context_step
            .min(crate::diffview::CONTEXT_STEPS.len() - 1),
        layout: match stored.layout.as_deref() {
            Some("split") => Some(crate::diffview::DiffLayout::Split),
            Some("unified") => Some(crate::diffview::DiffLayout::Unified),
            _ => None,
        },
        tree_visible: stored.tree_visible,
        single_file: stored.single_file,
    }
}

pub fn save_diff_preferences(preferences: crate::diffview::DiffPreferences) {
    save_state(move |state| {
        state.diff = DiffViewState {
            context_step: preferences.context_step,
            layout: preferences.layout.map(|layout| {
                match layout {
                    crate::diffview::DiffLayout::Split => "split",
                    crate::diffview::DiffLayout::Unified => "unified",
                }
                .to_owned()
            }),
            tree_visible: preferences.tree_visible,
            single_file: preferences.single_file,
        };
    });
}

#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ThreadViewState {
    pub viewport_offset: usize,
    pub expanded: Vec<String>,
}

pub fn load_state() -> TuiState {
    let Some(directory) = config_dir() else {
        return TuiState::default();
    };
    std::fs::read_to_string(directory.join("tui-state.toml"))
        .ok()
        .and_then(|content| toml::from_str(&content).ok())
        .unwrap_or_default()
}

/// Load-modify-write so independent fields never clobber each other.
pub fn save_state(update: impl FnOnce(&mut TuiState)) {
    // Each save reads, changes and rewrites the whole file: one at a time,
    // so two changes made together (a test run's threads) both land.
    static SAVING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _saving = SAVING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(directory) = config_dir() else {
        return;
    };
    let mut state = load_state();
    update(&mut state);
    let _ = std::fs::create_dir_all(&directory);
    if let Ok(body) = toml::to_string(&state) {
        write_whole(&directory.join("tui-state.toml"), &body);
    }
}

/// Write `body` to `path` so that a read never finds it half written: beside
/// it, then moved into place. A file kept elsewhere through a symbolic link
/// (a dotfiles folder) is written where the link leads, and the link stays.
fn write_whole(path: &std::path::Path, body: &str) {
    let target = std::fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
        .then(|| std::fs::canonicalize(path).ok())
        .flatten()
        .unwrap_or_else(|| path.to_path_buf());
    let name = target.file_name().map_or_else(
        || "tui-state.toml".into(),
        |name| name.to_string_lossy().into_owned(),
    );
    let partial = target.with_file_name(format!("{name}.{}.partial", std::process::id()));
    if std::fs::write(&partial, body).is_err() || std::fs::rename(&partial, &target).is_err() {
        let _ = std::fs::remove_file(&partial);
    }
}

/// The persisted theme selection (`tui-state.toml`), then the `[tui]` section
/// of config.toml, as (theme, variant).
pub fn load_theme_selection() -> (Option<String>, Option<String>) {
    #[derive(Default, serde::Deserialize)]
    #[serde(default)]
    struct TuiSection {
        theme: Option<String>,
        variant: Option<String>,
    }
    #[derive(Default, serde::Deserialize)]
    #[serde(default)]
    struct ConfigFile {
        tui: TuiSection,
    }
    let mut theme = None;
    let mut variant = None;
    if let Some(content) = client_config_text() {
        if let Ok(config) = toml::from_str::<ConfigFile>(&content) {
            theme = config.tui.theme;
            variant = config.tui.variant;
        }
    }
    let state = load_state();
    if state.theme.is_some() {
        theme = state.theme;
    }
    if state.variant.is_some() {
        variant = state.variant;
    }
    (theme, variant)
}

/// The `[tui].background_mode` setting. `auto` is the compatibility default:
/// exact RGB terminals keep the theme page color, while limited-color
/// terminals keep their own configured background until a theme is chosen.
pub fn load_background_mode() -> BackgroundMode {
    client_config_text()
        .and_then(|content| parse_background_mode(&content))
        .unwrap_or_default()
}

/// Optional `[tui.statusline]` command. String commands run through the
/// platform shell; argv commands bypass shell parsing. Invalid or empty
/// entries disable the feature without affecting TUI startup.
pub fn load_statusline() -> Option<StatusLineConfig> {
    #[derive(Default, serde::Deserialize)]
    #[serde(default)]
    struct TuiSection {
        statusline: Option<StatusLineConfig>,
    }
    #[derive(Default, serde::Deserialize)]
    #[serde(default)]
    struct ConfigFile {
        tui: TuiSection,
    }
    let content = client_config_text()?;
    let mut config = toml::from_str::<ConfigFile>(&content)
        .ok()?
        .tui
        .statusline?;
    let empty = match &config.command {
        StatusLineCommand::Shell(command) => command.trim().is_empty(),
        StatusLineCommand::Argv(command) => command.is_empty() || command[0].trim().is_empty(),
    };
    if empty {
        return None;
    }
    config.refresh_ms = config.refresh_ms.clamp(250, 60_000);
    config.timeout_ms = config.timeout_ms.clamp(100, 30_000);
    Some(config)
}

fn parse_background_mode(content: &str) -> Option<BackgroundMode> {
    #[derive(Default, serde::Deserialize)]
    #[serde(default)]
    struct TuiSection {
        background_mode: BackgroundMode,
    }
    #[derive(Default, serde::Deserialize)]
    #[serde(default)]
    struct ConfigFile {
        tui: TuiSection,
    }
    toml::from_str::<ConfigFile>(content)
        .ok()
        .map(|config| config.tui.background_mode)
}

/// Persist the live theme choice without touching the shared config.toml.
pub fn save_theme_selection(theme: &str, variant: &str) {
    let theme = theme.to_owned();
    let variant = variant.to_owned();
    save_state(move |state| {
        state.theme = Some(theme);
        state.variant = Some(variant);
    });
}

pub fn load_language() -> crate::locale::Language {
    #[derive(Default, serde::Deserialize)]
    #[serde(default)]
    struct TuiSection {
        language: Option<String>,
    }
    #[derive(Default, serde::Deserialize)]
    #[serde(default)]
    struct ConfigFile {
        tui: TuiSection,
    }
    let configured = client_config_text()
        .and_then(|content| toml::from_str::<ConfigFile>(&content).ok())
        .and_then(|config| config.tui.language);
    load_state()
        .language
        .as_deref()
        .and_then(crate::locale::Language::parse)
        .or_else(|| {
            configured
                .as_deref()
                .and_then(crate::locale::Language::parse)
        })
        .unwrap_or_default()
}

pub fn save_language(language: crate::locale::Language) {
    save_state(move |state| state.language = Some(language.code().to_owned()));
}

/// The thinking display chosen last, Compact until one is chosen.
pub fn load_thinking() -> crate::state::ThinkingMode {
    load_state()
        .thinking
        .as_deref()
        .and_then(crate::state::ThinkingMode::parse)
        .unwrap_or_default()
}

pub fn save_thinking(mode: crate::state::ThinkingMode) {
    save_state(move |state| state.thinking = Some(mode.key().to_owned()));
}

/// Whether the sidebar was left open where it docks; open until chosen.
pub fn load_sidebar() -> bool {
    load_state().sidebar.unwrap_or(true)
}

pub fn save_sidebar(open: bool) {
    save_state(move |state| state.sidebar = Some(open));
}

pub(crate) fn load_client_json<T: serde::de::DeserializeOwned + Default>(
    name: &str,
) -> anyhow::Result<T> {
    let directory = config_dir()
        .ok_or_else(|| anyhow::anyhow!("Client configuration directory is unavailable"))?;
    match std::fs::read(directory.join(name)) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(error.into()),
    }
}
pub(crate) fn save_client_json(name: &str, value: &impl serde::Serialize) -> anyhow::Result<()> {
    use std::io::Write;
    let directory = config_dir()
        .ok_or_else(|| anyhow::anyhow!("Client configuration directory is unavailable"))?;
    std::fs::create_dir_all(&directory)?;
    let temporary = directory.join(format!(".{name}-{}", uuid::Uuid::now_v7()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> anyhow::Result<()> {
        let mut file = options.open(&temporary)?;
        file.write_all(&serde_json::to_vec(value)?)?;
        file.sync_all()?;
        std::fs::rename(&temporary, directory.join(name))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

/// Hold a stable sidecar lock across reading and atomically replacing a JSON
/// document. Locking the document itself would lock the old inode after rename.
pub(crate) fn update_client_json<T: serde::de::DeserializeOwned + serde::Serialize + Default>(
    name: &str,
    update: impl FnOnce(&mut T),
) -> anyhow::Result<()> {
    use fs2::FileExt as _;
    let directory = config_dir()
        .ok_or_else(|| anyhow::anyhow!("Client configuration directory is unavailable"))?;
    std::fs::create_dir_all(&directory)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(directory.join(format!(".{name}.lock")))?;
    lock.lock_exclusive()?;
    let mut value = load_client_json::<T>(name)?;
    update(&mut value);
    save_client_json(name, &value)
}

/// The shared config.toml, as the TUI reads it: its `[tui]` settings are
/// laid over any that only the TUI's old folder still has, so moving to the
/// shared file never loses a setting.
pub(crate) fn client_config_text() -> Option<String> {
    let read = |directory: PathBuf| std::fs::read_to_string(directory.join("config.toml")).ok();
    merge_tui_settings(
        config_dir().and_then(read),
        legacy_config_dir().and_then(read),
    )
}

fn merge_tui_settings(current: Option<String>, legacy: Option<String>) -> Option<String> {
    let Some(legacy_tui) = legacy
        .as_deref()
        .and_then(|text| toml::from_str::<toml::Table>(text).ok())
        .and_then(|mut table| table.remove("tui"))
    else {
        return current;
    };
    let mut table = match current.as_deref().map(toml::from_str::<toml::Table>) {
        Some(Ok(table)) => table,
        // A broken shared file is reported by whoever reads it, as before.
        Some(Err(_)) => return current,
        None => toml::Table::new(),
    };
    let merged = match table.remove("tui") {
        Some(tui) => overlay(legacy_tui, tui),
        None => legacy_tui,
    };
    table.insert("tui".into(), merged);
    toml::to_string(&table).ok().or(current)
}

/// `over` wins, table by table.
fn overlay(base: toml::Value, over: toml::Value) -> toml::Value {
    match (base, over) {
        (toml::Value::Table(mut base), toml::Value::Table(over)) => {
            for (key, value) in over {
                let value = match base.remove(&key) {
                    Some(previous) => overlay(previous, value),
                    None => value,
                };
                base.insert(key, value);
            }
            toml::Value::Table(base)
        }
        (_, over) => over,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BackgroundMode, UnreadTheme, add_theme_entries, adopt_legacy_files, merge_tui_settings,
        parse_background_mode, resolve_config_dir, write_whole,
    };
    use std::path::PathBuf;

    /// A theme file that is not one is returned with the first line of why,
    /// for a notice, and the other themes still load. Nothing is printed: the
    /// TUI may be drawing on the terminal (a server switch reloads them).
    #[test]
    fn theme_files_that_cannot_be_read_are_returned_not_printed() {
        let directory =
            std::env::temp_dir().join(format!("haarness-themes-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("broken.toml"), "primary = [\n").unwrap();
        std::fs::write(
            directory.join("fine.toml"),
            "name = \"fine\"\nprimary = \"#336699\"\n",
        )
        .unwrap();
        let (mut entries, mut names, mut unread) =
            (Vec::new(), std::collections::HashSet::default(), Vec::new());
        add_theme_entries(&mut entries, &mut names, &mut unread, &directory);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["fine"]
        );
        assert_eq!(unread.len(), 1, "{unread:?}");
        let UnreadTheme { file, reason } = &unread[0];
        assert_eq!(file, "broken.toml");
        assert!(reason.starts_with("TOML parse error"), "{reason}");
        assert!(!reason.contains('\n'), "{reason}");
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// A state file kept elsewhere through a link is written where the link
    /// leads, the link stays a link, and no half-written copy is left behind.
    #[cfg(unix)]
    #[test]
    fn state_is_written_through_a_link_and_leaves_no_partial_file() {
        let root =
            std::env::temp_dir().join(format!("haarness-write-whole-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dotfiles = root.join("dotfiles");
        let config = root.join("config");
        std::fs::create_dir_all(&dotfiles).unwrap();
        std::fs::create_dir_all(&config).unwrap();
        let target = dotfiles.join("tui-state.toml");
        std::fs::write(&target, "old").unwrap();
        let link = config.join("tui-state.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        write_whole(&link, "sidebar = false\n");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "sidebar = false\n"
        );
        // A write that cannot land leaves nothing beside the file.
        let missing = root.join("gone").join("tui-state.toml");
        write_whole(&missing, "x");
        for folder in [&dotfiles, &config] {
            let names: Vec<_> = std::fs::read_dir(folder)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            assert!(
                names.iter().all(|name| !name.ends_with(".partial")),
                "{names:?}"
            );
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn config_folder_is_the_one_the_cli_uses_on_every_os() {
        let env = |pairs: &'static [(&str, &str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| std::ffi::OsString::from(value))
            }
        };
        let home = Some(PathBuf::from("/Users/someone"));
        // Not ~/Library/Application Support: the server reads ~/.config.
        assert_eq!(
            resolve_config_dir(env(&[]), home.clone()),
            Some(PathBuf::from("/Users/someone/.config/haarness"))
        );
        assert_eq!(
            resolve_config_dir(env(&[("XDG_CONFIG_HOME", "/xdg")]), home.clone()),
            Some(PathBuf::from("/xdg/haarness"))
        );
        // A relative XDG value is ignored, as the CLI ignores it.
        assert_eq!(
            resolve_config_dir(env(&[("XDG_CONFIG_HOME", "xdg")]), home.clone()),
            Some(PathBuf::from("/Users/someone/.config/haarness"))
        );
        assert_eq!(
            resolve_config_dir(
                env(&[("HAARNESS_HOME", "/opt/h"), ("XDG_CONFIG_HOME", "/xdg")]),
                home.clone()
            ),
            Some(PathBuf::from("/opt/h/config"))
        );
        assert_eq!(
            resolve_config_dir(
                env(&[("HAARNESS_DATA_DIR", "/data"), ("HAARNESS_HOME", "/opt/h")]),
                home
            ),
            Some(PathBuf::from("/data/config"))
        );
    }

    #[test]
    fn tui_settings_from_the_old_folder_survive_under_the_shared_ones() {
        let current = "[providers.ollama]\nenabled = false\n[tui]\ntheme = \"nord\"\n[tui.keybinds]\nmodels = \"F6\"\n";
        let legacy = "[tui]\ntheme = \"dracula\"\nbackground_mode = \"theme\"\n[tui.keybinds]\nagents = \"F7\"\nmodels = \"F8\"\n";
        let merged: toml::Table =
            toml::from_str(&merge_tui_settings(Some(current.into()), Some(legacy.into())).unwrap())
                .unwrap();
        let tui = &merged["tui"];
        assert_eq!(tui["theme"].as_str(), Some("nord"));
        assert_eq!(tui["background_mode"].as_str(), Some("theme"));
        assert_eq!(tui["keybinds"]["models"].as_str(), Some("F6"));
        assert_eq!(tui["keybinds"]["agents"].as_str(), Some("F7"));
        assert_eq!(
            merged["providers"]["ollama"]["enabled"].as_bool(),
            Some(false)
        );
        // Only the old file: its settings still apply.
        assert_eq!(
            parse_background_mode(&merge_tui_settings(None, Some(legacy.into())).unwrap()),
            Some(BackgroundMode::Theme)
        );
        // Nothing to merge, or a broken shared file: the text is left alone.
        assert_eq!(
            merge_tui_settings(Some("[tui".into()), Some(legacy.into())).as_deref(),
            Some("[tui")
        );
        assert_eq!(
            merge_tui_settings(Some(current.into()), None).as_deref(),
            Some(current)
        );
    }

    #[test]
    fn the_tui_files_move_to_the_shared_folder_without_overwriting() {
        let root =
            std::env::temp_dir().join(format!("haarness-tui-config-{}", uuid::Uuid::now_v7()));
        let legacy = root.join("legacy");
        let target = root.join("target");
        std::fs::create_dir_all(legacy.join("themes")).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(legacy.join("tui-state.toml"), "theme = \"old\"").unwrap();
        std::fs::write(legacy.join("tui-history-v2.json"), "[]").unwrap();
        std::fs::write(legacy.join("themes/mine.toml"), "x").unwrap();
        std::fs::write(legacy.join("auth.json"), "{}").unwrap();
        std::fs::write(target.join("tui-history-v2.json"), "[\"newer\"]").unwrap();

        adopt_legacy_files(&target, &legacy);

        assert_eq!(
            std::fs::read_to_string(target.join("tui-state.toml")).unwrap(),
            "theme = \"old\""
        );
        assert_eq!(
            std::fs::read_to_string(target.join("tui-history-v2.json")).unwrap(),
            "[\"newer\"]"
        );
        assert!(target.join("themes/mine.toml").is_file());
        // Only the TUI's own files: credentials are the CLI's to move.
        assert!(!target.join("auth.json").exists());
        assert!(legacy.join("tui-state.toml").is_file());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn background_mode_parses_all_supported_values_and_defaults_safely() {
        assert_eq!(
            parse_background_mode("[tui]\nbackground_mode = \"terminal\""),
            Some(BackgroundMode::Terminal)
        );
        assert_eq!(
            parse_background_mode("[tui]\nbackground_mode = \"theme\""),
            Some(BackgroundMode::Theme)
        );
        assert_eq!(parse_background_mode("[tui]"), Some(BackgroundMode::Auto));
        assert_eq!(
            parse_background_mode("[tui]\nbackground_mode = \"invalid\""),
            None
        );
    }
}
