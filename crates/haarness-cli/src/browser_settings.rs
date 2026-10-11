//! Live, revision-checked browser policy shared by Desktop and attached clients.
use crate::input_settings::{files, layers, project_files, target};
use anyhow::{Result, ensure};
use haarness_server::browser_policy::BrowserPolicy;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

fn read(path: &Path) -> Result<Option<BrowserPolicy>> {
    let raw = crate::jsonconfig::read_json(path).map_err(anyhow::Error::msg)?;
    let Some(value) = raw
        .as_ref()
        .and_then(|value| value.get("browser"))
        .and_then(|value| value.get("policy"))
    else {
        return Ok(None);
    };
    let mut policy: BrowserPolicy = serde_json::from_value(value.clone())?;
    policy.validate()?;
    Ok(Some(policy))
}
fn snapshot(project_config: bool, global: &Path, workspace: &Path, scope: &str) -> Result<Value> {
    let owned = if scope == "global" {
        files(global)
    } else {
        project_files(workspace)
    };
    let mut effective = BrowserPolicy::default();
    let mut inherited = effective.clone();
    let mut selected = None;
    let mut source = "default";
    let mut hash = Sha256::new();
    for path in layers(global, workspace)
        .into_iter()
        .filter(|path| project_config || files(global).contains(path))
    {
        hash.update(path.to_string_lossy().as_bytes());
        match fs::read(&path) {
            Ok(bytes) => hash.update(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => hash.update(b"missing"),
            Err(e) => return Err(e.into()),
        }
        if let Some(policy) = read(&path)? {
            effective = policy.clone();
            source = if files(global).contains(&path) {
                "global"
            } else {
                "project"
            };
            if owned.contains(&path) {
                selected = Some(policy.clone());
            }
            if scope == "project" && !owned.contains(&path) {
                inherited = policy;
            }
        }
    }
    Ok(
        json!({"policy":effective,"scopePolicy":selected,"inheritedPolicy":inherited,"source":source,"scope":scope,"revision":format!("{:x}",hash.finalize())}),
    )
}
pub(super) fn load(project_config: bool, global: &Path, workspace: &Path) -> Result<BrowserPolicy> {
    Ok(serde_json::from_value(
        snapshot(project_config, global, workspace, "project")?["policy"].clone(),
    )?)
}
pub(super) fn request(
    project_config: bool,
    global: &Path,
    workspace: &Path,
    input: &Value,
) -> Result<Value> {
    let scope = input["scope"].as_str().unwrap_or("project");
    let action = input["action"].as_str().unwrap_or("get");
    ensure!(
        matches!(scope, "project" | "global"),
        "Scope must be project or global"
    );
    ensure!(
        matches!(action, "get" | "set" | "reset"),
        "Unknown browser settings action"
    );
    let _lock = if action == "get" {
        None
    } else {
        fs::create_dir_all(global)?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(global.join("browser-settings.lock"))?;
        fs2::FileExt::lock_exclusive(&lock)?;
        Some(lock)
    };
    ensure!(
        project_config || scope == "global" || action == "get",
        "Project configuration is disabled for this server. Choose Global scope to change this setting."
    );
    let before = snapshot(project_config, global, workspace, scope)?;
    if action != "get" {
        ensure!(
            input["revision"]
                .as_str()
                .is_some_and(|revision| before["revision"] == revision),
            "Browser settings changed. Refresh before saving."
        );
        if action == "set" {
            let mut policy: BrowserPolicy = serde_json::from_value(input["policy"].clone())?;
            policy.validate()?;
            crate::jsonconfig::write_entry(
                &target(global, workspace, scope),
                "browser",
                "policy",
                Some(serde_json::to_value(policy)?),
            )?;
        } else {
            for path in if scope == "global" {
                files(global)
            } else {
                project_files(workspace)
            } {
                if read(&path)?.is_some() {
                    crate::jsonconfig::write_entry(&path, "browser", "policy", None)?;
                }
            }
        }
    }
    snapshot(project_config, global, workspace, scope)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_settings_inheritance_stale_writes_and_reset_preserve_other_settings() {
        let dir = std::env::temp_dir().join(format!(
            "haarness-browser-settings-{}",
            uuid::Uuid::now_v7()
        ));
        let global = dir.join("global");
        let workspace = dir.join("project");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(
            workspace.join("haarness.jsonc"),
            "{\n// keep this\n\"input\":{\"delivery\":\"queue\"}\n}",
        )
        .unwrap();
        let first = request(true, &global, &workspace, &json!({"scope":"global"})).unwrap();
        let mut policy = BrowserPolicy::default();
        policy.enabled = false;
        let disabled = request(
            true,
            &global,
            &workspace,
            &json!({"scope":"global","action":"set","revision":first["revision"],"policy":policy}),
        )
        .unwrap();
        assert!(!load(true, &global, &workspace).unwrap().enabled);
        assert!(
            request(
                true,
                &global,
                &workspace,
                &json!({"action":"set","revision":first["revision"],"policy":policy})
            )
            .is_err()
        );
        policy.enabled = true;
        let enabled = request(
            true,
            &global,
            &workspace,
            &json!({"action":"set","revision":disabled["revision"],"policy":policy}),
        )
        .unwrap();
        assert!(load(true, &global, &workspace).unwrap().enabled);
        request(
            true,
            &global,
            &workspace,
            &json!({"action":"reset","revision":enabled["revision"]}),
        )
        .unwrap();
        assert!(!load(true, &global, &workspace).unwrap().enabled);
        assert!(
            fs::read_to_string(workspace.join("haarness.jsonc"))
                .unwrap()
                .contains("keep this")
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
