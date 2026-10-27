//! Durable availability overrides, separate from installed source files.
use crate::Paths;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub fn sources(paths: &Paths, workspace: &Path, trust: bool) -> Vec<PathBuf> {
    let mut files = vec![paths.config_dir().join("capabilities.json")];
    if trust {
        files.extend(
            crate::layout::project_directories(workspace)
                .into_iter()
                .map(|p| p.join(".haarness/capabilities.json")),
        );
    }
    files
}

pub fn load(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
) -> Result<BTreeMap<String, BTreeMap<String, bool>>> {
    let mut result: BTreeMap<String, BTreeMap<String, bool>> = BTreeMap::new();
    for path in sources(paths, workspace, trust) {
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        let layer: BTreeMap<String, BTreeMap<String, bool>> = serde_json::from_slice(&bytes)
            .with_context(|| format!("Invalid capability settings: {}", path.display()))?;
        for (kind, entries) in layer {
            result.entry(kind).or_default().extend(entries);
        }
    }
    Ok(result)
}

pub fn disabled(paths: &Paths, workspace: &Path, trust: bool, kind: &str) -> Result<Vec<String>> {
    Ok(load(paths, workspace, trust)?
        .remove(kind)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(name, enabled)| (!enabled).then_some(name))
        .collect())
}

pub fn set(
    paths: &Paths,
    workspace: &Path,
    trust: bool,
    kind: &str,
    name: &str,
    enabled: bool,
    local: bool,
) -> Result<Value> {
    ensure!(
        matches!(kind, "skill" | "agent" | "mcp"),
        "Unsupported availability override"
    );
    ensure!(
        !name.is_empty() && name.len() <= 256,
        "Invalid capability name"
    );
    ensure!(!local || trust, "Project configuration is turned off");
    let path = if local {
        workspace.join(".haarness/capabilities.json")
    } else {
        paths.config_dir().join("capabilities.json")
    };
    let _lock = crate::desktop::editor_lock(&paths.data, "capabilities")?;
    haarness_profiles::check_path(&path)?;
    let mut value: Value = if path.exists() {
        serde_json::from_slice(&fs::read(&path)?)?
    } else {
        json!({})
    };
    ensure!(value.is_object(), "Invalid capability settings");
    if value.get(kind).is_none() {
        value[kind] = json!({});
    }
    ensure!(value[kind].is_object(), "Invalid capability settings");
    value[kind][name] = json!(enabled);
    haarness_profiles::write(&path, &serde_json::to_vec_pretty(&value)?, true)?;
    Ok(json!({"kind":kind,"name":name,"enabled":enabled,"restartRequired":false}))
}
