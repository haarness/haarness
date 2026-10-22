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
/// wrote) is skipped, and a file it cannot read, that is not a settings
/// object or holds a value out of range gives the defaults for what it
/// cannot use. Each such file is named in one WARN line. Saving takes the
/// strict form ([`MemorySettings`] refuses unknown keys).
pub fn load(root: &Path) -> MemorySettings {
    let path = root.join("settings.json");
    match std::fs::read(&path) {
        Ok(bytes) => {
            let (settings, problems) = lenient(&bytes);
            if !problems.is_empty() {
                warn_once(&path, &problems);
            }
            settings
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => MemorySettings::default(),
        Err(error) => {
            warn_once(
                &path,
                &[format!("cannot be read ({error}); using the defaults")],
            );
            MemorySettings::default()
        }
    }
}

/// The settings in `bytes`, and what could not be used of them.
fn lenient(bytes: &[u8]) -> (MemorySettings, Vec<String>) {
    let defaults = MemorySettings::default();
    let object = match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(serde_json::Value::Object(object)) => object,
        Ok(_) => {
            return (
                defaults,
                vec!["is not a JSON object; using the defaults".into()],
            );
        }
        Err(error) => {
            return (
                defaults,
                vec![format!("is not valid JSON ({error}); using the defaults")],
            );
        }
    };
    let mut settings = defaults.clone();
    let mut problems = Vec::new();
    for (key, value) in object {
        match key.as_str() {
            "enabled" => match value.as_bool() {
                Some(enabled) => settings.enabled = enabled,
                None => problems.push("enabled is not true or false; using true".into()),
            },
            "indexLimit" => match value.as_u64().and_then(|limit| usize::try_from(limit).ok()) {
                Some(limit) if (1..=1000).contains(&limit) => settings.index_limit = limit,
                _ => problems.push(format!(
                    "indexLimit is not between 1 and 1000; using {}",
                    defaults.index_limit
                )),
            },
            _ => problems.push(format!(
                "ignored {}, a key Haarness does not know",
                haarness_core::redact::log_text(&key, 80)
            )),
        }
    }
    (settings, problems)
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

    fn read(text: &str) -> (MemorySettings, Vec<String>) {
        lenient(text.as_bytes())
    }

    #[test]
    fn a_settings_file_haarness_cannot_fully_use_still_loads() {
        let (settings, problems) =
            read(r#"{"enabled": false, "indexLimit": 50, "summaryModel": "newer-version"}"#);
        assert!(!settings.enabled);
        assert_eq!(settings.index_limit, 50);
        assert_eq!(
            problems,
            ["ignored summaryModel, a key Haarness does not know"]
        );
        for (text, problem) in [
            ("{\"enabled\": fals", "is not valid JSON"),
            ("[1, 2]", "is not a JSON object"),
        ] {
            let (settings, problems) = read(text);
            assert!(settings.enabled && settings.index_limit == 200, "{text}");
            assert!(problems[0].starts_with(problem), "{problems:?}");
        }
        let (settings, problems) = read(r#"{"enabled": "no", "indexLimit": 5000}"#);
        assert!(settings.enabled);
        assert_eq!(settings.index_limit, 200);
        assert_eq!(problems.len(), 2, "{problems:?}");
    }

    #[test]
    fn a_runtime_reads_a_settings_file_with_a_newer_key() {
        let root = std::env::temp_dir().join(format!("haarness-memory-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("settings.json"),
            r#"{"enabled": false, "futureKey": 1}"#,
        )
        .unwrap();
        assert!(!load(&root).enabled);
        std::fs::write(root.join("settings.json"), "not json").unwrap();
        assert!(load(&root).enabled);
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
