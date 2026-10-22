use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct MemorySettings {
    pub enabled: bool,
    pub index_limit: usize,
}
impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            index_limit: 200,
        }
    }
}
impl MemorySettings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=1000).contains(&self.index_limit),
            "Index limit must be between 1 and 1000"
        );
        Ok(())
    }
    pub fn apply(&self, config: &mut haarness_core::RuntimeConfig) {
        if !self.enabled {
            for tool in [
                "memory_list",
                "memory_read",
                "memory_search",
                "memory_save",
                "memory_delete",
            ] {
                config.tool_availability.insert(tool.into(), false);
            }
        }
    }
}
/// The memory settings saved in `root`, read leniently, since the runtime
/// is built from them: a key Haarness does not know (one a newer version
/// wrote) is skipped and an out-of-range index limit gives the default. A
/// file that exists but cannot be read, is not a settings object or says
/// `enabled` with something other than true or false keeps memory off, so a
/// broken file never turns memory back on. Each file's problems are named
/// once in a WARN line. Saving takes the strict form ([`MemorySettings`]
/// refuses unknown keys).
pub fn load(root: &Path) -> MemorySettings {
    read(root).settings
}

/// What [`load`] read, and whether the file was there but could not be used,
/// which leaves memory off until the settings are saved again.
pub struct Loaded {
    pub settings: MemorySettings,
    pub unreadable: bool,
}

pub fn read(root: &Path) -> Loaded {
    let path = root.join("settings.json");
    let read = match std::fs::read(&path) {
        Ok(bytes) => lenient(&bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Lenient::default(),
        Err(error) => Lenient::closed(format!("cannot be read ({error})")),
    };
    if !read.problems.is_empty() {
        warn_once(&path, &read.problems);
    }
    Loaded {
        settings: read.settings,
        unreadable: read.unreadable,
    }
}

#[derive(Default)]
struct Lenient {
    settings: MemorySettings,
    problems: Vec<String>,
    unreadable: bool,
}

impl Lenient {
    /// A file memory cannot trust: off, and said so.
    fn closed(problem: String) -> Self {
        Self {
            settings: MemorySettings {
                enabled: false,
                ..MemorySettings::default()
            },
            problems: vec![format!(
                "{problem}; memory stays off until the settings are saved"
            )],
            unreadable: true,
        }
    }
}

/// The settings in `bytes`, and what could not be used of them.
fn lenient(bytes: &[u8]) -> Lenient {
    let object = match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(serde_json::Value::Object(object)) => object,
        Ok(_) => return Lenient::closed("is not a JSON object".into()),
        Err(error) => return Lenient::closed(format!("is not valid JSON ({error})")),
    };
    let defaults = MemorySettings::default();
    let mut read = Lenient::default();
    for (key, value) in object {
        match key.as_str() {
            "enabled" => match value.as_bool() {
                Some(enabled) => read.settings.enabled = enabled,
                None => {
                    read.settings.enabled = false;
                    read.unreadable = true;
                    read.problems.push(
                        "enabled is not true or false; memory stays off until the settings are saved"
                            .into(),
                    );
                }
            },
            "indexLimit" => match value.as_u64().and_then(|limit| usize::try_from(limit).ok()) {
                Some(limit) if (1..=1000).contains(&limit) => read.settings.index_limit = limit,
                _ => read.problems.push(format!(
                    "indexLimit is not between 1 and 1000; using {}",
                    defaults.index_limit
                )),
            },
            _ => read.problems.push(format!(
                "ignored {}, a key Haarness does not know",
                haarness_core::redact::log_text(&key, 80)
            )),
        }
    }
    read
}

/// One WARN line per file and problem set, however often the settings are
/// read (every runtime build and memory view reads them).
fn warn_once(path: &Path, problems: &[String]) {
    static WARNED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let line = problems.join("; ");
    let key = format!("{}\0{line}", path.display());
    let mut warned = WARNED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !warned.contains(&key) {
        warned.push(key);
        tracing::warn!(path = %path.display(), "memory settings: {line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_text(text: &str) -> Lenient {
        lenient(text.as_bytes())
    }

    #[test]
    fn an_unknown_key_is_skipped_and_the_rest_is_used() {
        let read =
            read_text(r#"{"enabled": false, "indexLimit": 50, "summaryModel": "newer-version"}"#);
        assert!(!read.settings.enabled && !read.unreadable);
        assert_eq!(read.settings.index_limit, 50);
        assert_eq!(
            read.problems,
            ["ignored summaryModel, a key Haarness does not know"]
        );
        let read = read_text(r#"{"indexLimit": 5000}"#);
        assert!(read.settings.enabled && !read.unreadable);
        assert_eq!(read.settings.index_limit, 200);
    }

    /// A broken file never turns memory back on.
    #[test]
    fn a_settings_file_memory_cannot_trust_keeps_memory_off() {
        for (text, problem) in [
            (r#"{"enabled": false,}"#, "is not valid JSON"),
            ("[1, 2]", "is not a JSON object"),
            (r#"{"enabled": "false"}"#, "enabled is not true or false"),
        ] {
            let read = read_text(text);
            assert!(!read.settings.enabled && read.unreadable, "{text}");
            assert!(read.problems[0].starts_with(problem), "{:?}", read.problems);
        }
    }

    #[test]
    fn a_runtime_reads_a_settings_file_with_a_newer_key() {
        let root = std::env::temp_dir().join(format!("haarness-memory-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        assert!(load(&root).enabled, "no file: the defaults");
        std::fs::write(
            root.join("settings.json"),
            r#"{"enabled": true, "futureKey": 1}"#,
        )
        .unwrap();
        assert!(load(&root).enabled);
        std::fs::write(root.join("settings.json"), r#"{"enabled": false,}"#).unwrap();
        let read = read(&root);
        assert!(!read.settings.enabled && read.unreadable);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn saving_still_refuses_a_key_it_does_not_know() {
        assert!(
            serde_json::from_value::<MemorySettings>(
                serde_json::json!({"enabled": true, "futureKey": 1})
            )
            .is_err()
        );
    }
}
