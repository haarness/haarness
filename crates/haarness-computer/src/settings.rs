//! Persisted computer use settings: the on/off switch, the accepted terms and the app rules. They are machine-wide,
//! kept as `computer-use.json` in the global configuration folder, where an agent's file tools need a person's
//! approval to make changes.

use crate::policy::{self, Rule};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

pub const FILE_NAME: &str = "computer-use.json";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Agreement {
    pub version: String,
    /// RFC 3339.
    pub accepted_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// The person's switch. Agents use the screen only while it is on and the current terms are agreed.
    pub enabled: bool,
    pub terms: Option<Agreement>,
    /// What happens in an app without a rule: `ask` (the default) or `allow`.
    pub default_act: Rule,
    /// Briefly highlight each click target on screen.
    pub ring: bool,
    /// Rules by bundle identifier (`com.apple.Notes`, or `com.jetbrains.*`).
    pub apps: BTreeMap<String, Rule>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            terms: None,
            default_act: Rule::Ask,
            ring: true,
            apps: BTreeMap::new(),
        }
    }
}

impl Settings {
    #[must_use]
    pub fn accepted(&self) -> bool {
        self.terms
            .as_ref()
            .is_some_and(|terms| terms.version == crate::terms::VERSION)
    }

    /// Whether the person agreed to earlier terms than today's, which changed since: clients say so above the terms.
    #[must_use]
    pub fn terms_changed(&self) -> bool {
        self.terms.is_some() && !self.accepted()
    }

    /// On with the current terms agreed.
    #[must_use]
    pub fn on(&self) -> bool {
        self.enabled && self.accepted()
    }

    /// The rule for `bundle`: an exact rule first, then the longest matching prefix rule.
    #[must_use]
    pub fn rule(&self, bundle: &str) -> Option<Rule> {
        if let Some(rule) = self.apps.get(bundle) {
            return Some(*rule);
        }
        self.apps
            .iter()
            .filter(|(pattern, _)| pattern.ends_with(".*") && policy::matches(bundle, pattern))
            .max_by_key(|(pattern, _)| pattern.len())
            .map(|(_, rule)| *rule)
    }

    #[must_use]
    pub fn denied(&self) -> Vec<&str> {
        self.apps
            .iter()
            .filter(|(_, rule)| **rule == Rule::Deny)
            .map(|(bundle, _)| bundle.as_str())
            .collect()
    }

    /// Drops rules whose keys are not bundle identifiers, and a default of `deny`, which would make every app
    /// invisible without saying so.
    fn sanitized(mut self) -> Self {
        self.apps
            .retain(|bundle, _| policy::valid_bundle(bundle, true));
        if self.default_act == Rule::Deny {
            self.default_act = Rule::Ask;
        }
        self
    }
}

/// When the file last changed and how long it was: a different stamp means read it again.
type Stamp = Option<(SystemTime, u64)>;

/// Reads and writes the settings file, re-reading it whenever it changed on disk.
pub struct Store {
    path: PathBuf,
    cache: Mutex<Option<(Stamp, Settings)>>,
    write: Mutex<()>,
}

impl Store {
    #[must_use]
    pub fn new(config_dir: &Path) -> Self {
        Self {
            path: config_dir.join(FILE_NAME),
            cache: Mutex::new(None),
            write: Mutex::new(()),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn stamp(&self) -> Stamp {
        let meta = fs::metadata(&self.path).ok()?;
        Some((meta.modified().ok()?, meta.len()))
    }

    /// The settings now: off when the file is missing or unreadable.
    #[must_use]
    pub fn get(&self) -> Settings {
        let stamp = self.stamp();
        if let Some((cached, settings)) = self.cache.lock().as_ref()
            && *cached == stamp
        {
            return settings.clone();
        }
        let settings = match stamp {
            None => Settings::default(),
            Some(_) => {
                match fs::read(&self.path).map(|bytes| serde_json::from_slice::<Settings>(&bytes)) {
                    Ok(Ok(settings)) => settings.sanitized(),
                    Ok(Err(error)) => {
                        tracing::warn!(%error, path = %self.path.display(), "ignoring unreadable computer use settings (computer use stays off)");
                        Settings::default()
                    }
                    Err(error) => {
                        tracing::warn!(%error, path = %self.path.display(), "ignoring unreadable computer use settings (computer use stays off)");
                        Settings::default()
                    }
                }
            }
        };
        *self.cache.lock() = Some((stamp, settings.clone()));
        settings
    }

    /// Changes the settings with `change`, one writer at a time, and returns them.
    pub fn update(&self, change: impl FnOnce(&mut Settings)) -> io::Result<Settings> {
        let _guard = self.write.lock();
        let mut settings = self.get();
        change(&mut settings);
        let settings = settings.sanitized();
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Write a temporary file and rename it over the old one, so readers never see a partial file.
        let temp = self
            .path
            .with_extension(format!("json.{}.tmp", uuid::Uuid::now_v7()));
        let result = (|| {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            let mut file = options.open(&temp)?;
            file.write_all(&serde_json::to_vec_pretty(&settings).map_err(io::Error::other)?)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&temp, &self.path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result?;
        *self.cache.lock() = Some((self.stamp(), settings.clone()));
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_broken_files_read_as_off() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        assert_eq!(store.get(), Settings::default());
        fs::write(store.path(), "{not json").unwrap();
        assert!(!store.get().on());
    }

    #[test]
    fn on_needs_the_current_terms() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let settings = store.update(|settings| settings.enabled = true).unwrap();
        assert!(!settings.on());
        assert!(!settings.terms_changed(), "never agreed: nothing changed");
        // Agreed to the first terms, before Auto's reviewer could answer: off until today's are agreed, and clients
        // say the terms changed.
        let settings = store
            .update(|settings| {
                settings.terms = Some(Agreement {
                    version: "1".into(),
                    accepted_at: "2026-01-01T00:00:00Z".into(),
                });
            })
            .unwrap();
        assert!(!settings.on());
        assert!(settings.terms_changed());
        let settings = store
            .update(|settings| {
                settings.terms = Some(Agreement {
                    version: crate::terms::VERSION.into(),
                    accepted_at: "2026-01-01T00:00:00Z".into(),
                });
            })
            .unwrap();
        assert!(settings.on());
        assert!(!settings.terms_changed());
        // Another process changed the file: the next read sees it.
        let mut changed = settings.clone();
        changed.enabled = false;
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(store.path(), serde_json::to_vec_pretty(&changed).unwrap()).unwrap();
        assert!(!store.get().on());
    }

    #[test]
    fn rules_match_exactly_then_by_the_longest_prefix() {
        let mut settings = Settings::default();
        settings.apps.insert("com.jetbrains.*".into(), Rule::Ask);
        settings
            .apps
            .insert("com.jetbrains.intellij.*".into(), Rule::Deny);
        settings.apps.insert("com.apple.Notes".into(), Rule::Allow);
        settings.apps.insert("not a bundle".into(), Rule::Allow);
        settings.default_act = Rule::Deny;
        let settings = settings.sanitized();
        assert_eq!(settings.rule("com.apple.Notes"), Some(Rule::Allow));
        assert_eq!(settings.rule("com.jetbrains.goland"), Some(Rule::Ask));
        assert_eq!(settings.rule("com.jetbrains.intellij.ce"), Some(Rule::Deny));
        assert_eq!(settings.rule("com.apple.TextEdit"), None);
        assert!(!settings.apps.contains_key("not a bundle"));
        assert_eq!(settings.default_act, Rule::Ask);
    }
}
