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

fn config_dir() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("HAARNESS_DATA_DIR") {
        return Some(PathBuf::from(root).join("config"));
    }
    directories::ProjectDirs::from("dev", "haarness", "haarness")
        .map(|project| project.config_dir().to_path_buf())
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
    let path = root.join("tui-history-v2.json");
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
    let path = root.join("tui-history-v2.json");
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

/// Themes discovered from `<config>/themes/*.toml`, appended after the
/// built-ins. Unparseable files are skipped with a stderr note.
pub fn load_theme_entries() -> Vec<crate::theme::ThemeEntry> {
    let mut entries = crate::theme::builtin_entries();
    let Some(directory) = config_dir().map(|directory| directory.join("themes")) else {
        return entries;
    };
    let Ok(files) = std::fs::read_dir(directory) else {
        return entries;
    };
    let mut names: std::collections::HashSet<String> =
        entries.iter().map(|entry| entry.name.clone()).collect();
    for file in files.flatten() {
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
            Err(error) => eprintln!("Theme {}: {error}", path.display()),
        }
    }
    entries
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
    pub last_thread: Option<String>,
    /// Legacy single-thread scroll value; read as a fallback for two release
    /// cycles after per-thread view state shipped.
    pub viewport_offset: Option<usize>,
    pub thread_views: std::collections::BTreeMap<String, ThreadViewState>,
    pub diff: DiffViewState,
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
    let Some(directory) = config_dir() else {
        return;
    };
    let mut state = load_state();
    update(&mut state);
    let _ = std::fs::create_dir_all(&directory);
    if let Ok(body) = toml::to_string(&state) {
        let _ = std::fs::write(directory.join("tui-state.toml"), body);
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
    if let Some(directory) = config_dir() {
        if let Ok(content) = std::fs::read_to_string(directory.join("config.toml")) {
            if let Ok(config) = toml::from_str::<ConfigFile>(&content) {
                theme = config.tui.theme;
                variant = config.tui.variant;
            }
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
/// terminals keep their own configured background.
pub fn load_background_mode() -> BackgroundMode {
    let Some(directory) = config_dir() else {
        return BackgroundMode::default();
    };
    std::fs::read_to_string(directory.join("config.toml"))
        .ok()
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
    let directory = config_dir()?;
    let content = std::fs::read_to_string(directory.join("config.toml")).ok()?;
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
    let configured = config_dir()
        .and_then(|directory| std::fs::read_to_string(directory.join("config.toml")).ok())
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

pub(crate) fn client_config_text() -> Option<String> {
    std::fs::read_to_string(config_dir()?.join("config.toml")).ok()
}

#[cfg(test)]
mod tests {
    use super::{BackgroundMode, parse_background_mode};

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
