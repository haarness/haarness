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
pub fn load(root: &Path) -> Result<MemorySettings> {
    let path = root.join("settings.json");
    let value: MemorySettings = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => MemorySettings::default(),
        Err(error) => return Err(error.into()),
    };
    value.validate()?;
    Ok(value)
}
