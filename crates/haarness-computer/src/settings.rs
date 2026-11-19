//! Persisted computer use settings: the on/off switch and the app rules. They are machine-wide, kept as
//! `computer-use.json` in the global configuration folder, where an agent's file tools need a person's approval to
//! make changes.

use crate::policy::{self, Rule};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

pub const FILE_NAME: &str = "computer-use.json";

/// Earlier builds kept computer use off until the person accepted versioned terms, recorded in this file as
/// `terms: {"version", "acceptedAt"}`, and the last version they asked for was this one. This build has no terms: it
/// keeps whatever record the file holds, and switching on writes this version when the file holds no such record, so
/// an earlier build reading the same file (a server not yet restarted after an update, or a downgrade) sees computer
/// use on as well, rather than off behind terms. A file whose switch is on beside any other record was off for those
/// builds, and reads as off here too (see [`Settings::settle`]). The status reports the terms as accepted for earlier
/// clients.
pub(crate) const EARLIER_TERMS_VERSION: &str = "2";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// The person's switch: agents use the screen only while it is on.
    pub enabled: bool,
    /// The record earlier builds look for before they use the screen (see [`EARLIER_TERMS_VERSION`]). This build
    /// reads it only to settle a file they wrote.
    #[serde(rename = "terms", skip_serializing_if = "Option::is_none")]
    earlier_terms: Option<Value>,
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
            earlier_terms: None,
            default_act: Rule::Ask,
            ring: true,
            apps: BTreeMap::new(),
        }
    }
}

impl Settings {
    /// Turns the switch on or off. On also leaves the record earlier builds need to agree (see
    /// [`EARLIER_TERMS_VERSION`]); off leaves it as it is, since the switch alone says off to every build.
    pub fn switch(&mut self, on: bool) {
        self.enabled = on;
        if on && !self.earlier_terms_current() {
            self.earlier_terms = Some(json!({
                "version": EARLIER_TERMS_VERSION,
                "acceptedAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            }));
        }
    }

    /// Whether the file holds the record earlier builds accept today: version 2, with the date they read too.
    fn earlier_terms_current(&self) -> bool {
        self.earlier_terms.as_ref().is_some_and(|terms| {
            terms["version"] == EARLIER_TERMS_VERSION && terms["acceptedAt"].is_string()
        })
    }

    /// Switches off a file whose switch is on beside a record earlier builds no longer accept (an agreement to terms
    /// they replaced since, or one they cannot read): they kept computer use off, so it stays off rather than coming
    /// back on by itself, until the person switches it on. A file with no record follows its switch, as no build ever
    /// wrote one switched on. Says whether it switched off.
    fn settle(&mut self) -> bool {
        if self.enabled && self.earlier_terms.is_some() && !self.earlier_terms_current() {
            self.enabled = false;
            return true;
        }
        false
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
        let mut stamp = self.stamp();
        if let Some((cached, settings)) = self.cache.lock().as_ref()
            && *cached == stamp
        {
            return settings.clone();
        }
        let settings = match stamp {
            None => Settings::default(),
            Some(_) => {
                match fs::read(&self.path).map(|bytes| serde_json::from_slice::<Settings>(&bytes)) {
                    Ok(Ok(settings)) => {
                        let mut settings = settings.sanitized();
                        // Saved off, so the file says what every build reads. A change under way saves it anyway.
                        if settings.settle()
                            && let Some(_guard) = self.write.try_lock()
                        {
                            match self.write_file(&settings) {
                                Ok(()) => stamp = self.stamp(),
                                Err(error) => {
                                    tracing::warn!(%error, path = %self.path.display(), "could not save computer use as off");
                                }
                            }
                        }
                        settings
                    }
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
        self.write_file(&settings)?;
        *self.cache.lock() = Some((self.stamp(), settings.clone()));
        Ok(settings)
    }

    /// Writes `settings` over the file. Callers hold the write lock.
    fn write_file(&self, settings: &Settings) -> io::Result<()> {
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
            file.write_all(&serde_json::to_vec_pretty(settings).map_err(io::Error::other)?)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&temp, &self.path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
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
        assert!(!store.get().enabled);
    }

    #[test]
    fn a_file_reads_as_on_or_off_as_the_release_before_read_it_and_one_switch_turns_it_on() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let saved =
            || -> Value { serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap() };
        let write = |file: &Value| {
            // Another length or another moment: the next read sees the change.
            std::thread::sleep(std::time::Duration::from_millis(20));
            fs::write(store.path(), file.to_string()).unwrap();
        };
        let current = json!({"version": "2", "acceptedAt": "2026-11-07T00:00:00Z"});
        // On with today's record, off with any record, and on with none (no build wrote that, so the switch says).
        for (file, on) in [
            (json!({"enabled": true, "terms": current}), true),
            (json!({"enabled": false, "terms": current}), false),
            (
                json!({"enabled": false, "terms": {"version": "1", "acceptedAt": "2026-01-01T00:00:00Z"}}),
                false,
            ),
            (json!({"enabled": false}), false),
            (json!({"enabled": true}), true),
        ] {
            write(&file);
            assert_eq!(store.get().enabled, on, "{file}");
            assert_eq!(saved(), file, "nothing to save: {file}");
        }
        // On beside a record the release before no longer accepted, or could not read: it was off there, so it is
        // off here, and saved off with the record as it was.
        for terms in [
            json!({"version": "1", "acceptedAt": "2026-01-01T00:00:00Z"}),
            json!({"version": "2"}),
            json!("x"),
        ] {
            write(&json!({"enabled": true, "terms": terms}));
            assert!(!store.get().enabled, "{terms}");
            assert_eq!(saved()["enabled"], false, "{terms}");
            assert_eq!(saved()["terms"], terms);
            // One step turns it on, with the record earlier builds accept.
            let on = store.update(|settings| settings.switch(true)).unwrap();
            assert!(on.enabled);
            assert!(store.get().enabled);
            assert_eq!(saved()["terms"]["version"], "2");
            assert!(saved()["terms"]["acceptedAt"].is_string());
        }
        // Another process changing the file is seen on the next read.
        let mut settings = store.get();
        settings.enabled = false;
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(store.path(), serde_json::to_vec_pretty(&settings).unwrap()).unwrap();
        assert!(!store.get().enabled);
    }

    #[test]
    fn a_change_under_way_saves_the_settled_switch_too() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        fs::write(
            store.path(),
            json!({"enabled": true, "terms": {"version": "1", "acceptedAt": "2026-01-01T00:00:00Z"}}).to_string(),
        )
        .unwrap();
        let settings = store.update(|settings| settings.ring = false).unwrap();
        assert!(!settings.enabled);
        let file: Value = serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
        assert_eq!(file["enabled"], false);
        assert_eq!(file["ring"], false);
    }

    /// How earlier builds read the file: on only with `enabled` and an agreement to version 2.
    fn on_for_an_earlier_build(store: &Store) -> bool {
        let file: Value = serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
        file["enabled"] == true
            && file["terms"]["version"] == "2"
            && file["terms"]["acceptedAt"].is_string()
    }

    #[test]
    fn an_earlier_build_sharing_the_file_agrees_with_the_switch() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        // Never agreed anywhere: switching on here is on for an earlier build too.
        store.update(|settings| settings.switch(true)).unwrap();
        assert!(on_for_an_earlier_build(&store));
        // Off is off for both, and switching on again keeps the record as it was.
        let kept = fs::read_to_string(store.path()).unwrap();
        store.update(|settings| settings.switch(false)).unwrap();
        assert!(!on_for_an_earlier_build(&store));
        store.update(|settings| settings.switch(true)).unwrap();
        assert_eq!(fs::read_to_string(store.path()).unwrap(), kept);
        // An agreement to the first terms stays as it was through other changes, and switching on brings it up to
        // the version earlier builds ask for.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let first = json!({"version": "1", "acceptedAt": "2026-01-01T00:00:00Z"});
        fs::write(
            store.path(),
            json!({"enabled": false, "terms": first}).to_string(),
        )
        .unwrap();
        store.update(|settings| settings.ring = false).unwrap();
        let file: Value = serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
        assert_eq!(file["terms"], first);
        store.update(|settings| settings.switch(true)).unwrap();
        assert!(on_for_an_earlier_build(&store));
        // A file that never had a record gets none from changes that do not switch on.
        let fresh = tempfile::tempdir().unwrap();
        let store = Store::new(fresh.path());
        store.update(|settings| settings.ring = false).unwrap();
        let file: Value = serde_json::from_slice(&fs::read(store.path()).unwrap()).unwrap();
        assert!(file.get("terms").is_none(), "{file}");
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
