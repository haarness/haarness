//! Desktop hook edits use the runtime loader's sources and preserve unrelated config.
use crate::{Paths, asset_layers, jsonconfig};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

pub const EVENTS: &[&str] = &[
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "UserPromptSubmit",
    "PermissionRequest",
    "Stop",
    "PostCompact",
    "StopFailure",
    "SessionStart",
    "SessionEnd",
    "TurnEnd",
    "Notification",
    "FileChanged",
    "ConfigChange",
];
fn hash(value: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(value.as_ref()))
}
fn revision(path: &Path) -> Result<String> {
    Ok(hash(fs::read(path)?))
}
fn id(path: &Path, event: &str, group: usize, hook: usize) -> String {
    hash(format!("{}\0{event}\0{group}\0{hook}", path.display()))
}
fn rows(source: &jsonconfig::HookSource, workspace: &Path) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    let revision = revision(&source.path)?;
    if let Some(events) = source.block.as_object() {
        for (event, groups) in events {
            for (gi, group) in groups.as_array().into_iter().flatten().enumerate() {
                for (hi, hook) in group["hooks"].as_array().into_iter().flatten().enumerate() {
                    if hook
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("command")
                        != "command"
                    {
                        continue;
                    }
                    rows.push(json!({"id": id(&source.path,event,gi,hi), "event": event,
                        "matcher": group["matcher"].as_str().unwrap_or("*"), "handler": hook,
                        "revision": revision, "groupDisabled": group["disabled"].as_bool().unwrap_or(false),
                        "source": {"path": source.path, "editable": !source.plugin && !source.base,
                            "scope": if source.plugin {"plugin"} else if source.base {"base"} else if workspace.starts_with(source.path.parent().unwrap_or(workspace)) || source.path.starts_with(workspace) {"local"} else {"global"},
                            "trusted": source.trusted}}));
                }
            }
        }
    }
    Ok(rows)
}
fn validated(input: &Value, prior: Option<&Value>) -> Result<(String, String, Value)> {
    let event = input["event"].as_str().context("Choose a hook event")?;
    ensure!(EVENTS.contains(&event), "Unsupported hook event");
    let command = input["command"].as_str().context("Enter a command")?;
    ensure!(
        !command.trim().is_empty() && command.len() <= 65536 && !command.contains('\0'),
        "Enter a valid command"
    );
    let matcher = input["matcher"].as_str().unwrap_or("*").trim();
    ensure!(matcher.len() <= 1000, "Matcher is too long");
    let timeout = input["timeout"]
        .as_u64()
        .context("Timeout must be a whole number")?;
    ensure!(
        (1..=600).contains(&timeout),
        "Timeout must be between 1 and 600 seconds"
    );
    let args = input.get("args").cloned().unwrap_or(json!([]));
    ensure!(
        args.as_array().is_some_and(|a| a.len() <= 100
            && a.iter().all(|v| v
                .as_str()
                .is_some_and(|s| !s.contains('\0') && s.len() <= 65536))),
        "Invalid command arguments"
    );
    let mut handler = prior.cloned().unwrap_or(json!({}));
    handler["type"] = json!("command");
    handler["command"] = json!(command);
    handler["timeout"] = json!(timeout);
    handler["args"] = args;
    handler["async"] = json!(input["async"].as_bool().unwrap_or(false));
    let shell = input["shell"].as_str().unwrap_or("");
    ensure!(
        shell.len() <= 1000 && !shell.contains('\0'),
        "Invalid shell"
    );
    if shell.is_empty() {
        handler.as_object_mut().unwrap().remove("shell");
    } else {
        handler["shell"] = json!(shell);
    }
    Ok((
        event.into(),
        if matcher.is_empty() {
            "*".into()
        } else {
            matcher.into()
        },
        handler,
    ))
}
pub fn execute(paths: &Paths, workspace: &Path, trust: bool, input: &Value) -> Result<Value> {
    let action = input["action"].as_str().context("Missing hook action")?;
    let _lock = haarness_profiles::configuration_lock(&paths.data)?;
    let _editor_lock = crate::desktop::editor_lock(&paths.data, "hooks")?;
    let layers = asset_layers(paths, workspace, trust);
    if action == "list" {
        let items = layers
            .hook_sources
            .iter()
            .map(|source| rows(source, workspace))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        return Ok(
            json!({"items":items,"events":EVENTS,"errors":layers.errors,"projectTrusted":trust}),
        );
    }
    if action == "create" {
        let (event, matcher, handler) = validated(input, None)?;
        let root = match input["scope"].as_str() {
            Some("local") => workspace.to_path_buf(),
            Some("global") => paths.config_dir().to_path_buf(),
            _ => bail!("Choose project or global scope"),
        };
        let path = if root.join("haarness.json").exists() {
            root.join("haarness.json")
        } else if root.join("haarness.jsonc").exists() {
            root.join("haarness.jsonc")
        } else {
            root.join("haarness.json")
        };
        haarness_profiles::check_path(&path)?;
        let value = jsonconfig::read_json(&path)
            .map_err(anyhow::Error::msg)?
            .unwrap_or(json!({}));
        ensure!(
            value.get("hooks").is_none_or(Value::is_object),
            "Hooks configuration must be an object"
        );
        let existing = value.get("hooks").and_then(|v| v.get(&event));
        ensure!(
            existing.is_none_or(Value::is_array),
            "Hook event must contain a list"
        );
        let mut groups = existing
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        groups.push(json!({"matcher":matcher,"hooks":[handler]}));
        jsonconfig::write_entry(&path, "hooks", &event, Some(json!(groups)))?;
        return Ok(json!({"saved":true}));
    }
    ensure!(
        ["update", "toggle", "delete"].contains(&action),
        "Unsupported hook action"
    );
    let wanted = input["id"].as_str().context("Missing hook id")?;
    for source in layers.hook_sources {
        for row in rows(&source, workspace)? {
            if row["id"] != wanted {
                continue;
            }
            ensure!(
                !source.plugin,
                "Plugin hooks are read-only; manage the contributing plugin instead"
            );
            ensure!(
                !source.base,
                "Hooks from the base folder are read-only; Haarness does not change that folder"
            );
            haarness_profiles::check_path(&source.path)?;
            ensure!(
                input["revision"] == row["revision"],
                "Hooks changed outside this editor. Refresh before saving."
            );
            let mut block = source.block;
            let events = block.as_object_mut().context("Invalid hooks")?;
            let old_event = row["event"].as_str().unwrap().to_owned();
            let groups = events
                .get_mut(&old_event)
                .and_then(Value::as_array_mut)
                .context("Invalid hook groups")?;
            let (gi, hi) = groups
                .iter()
                .enumerate()
                .find_map(|(gi, g)| {
                    g["hooks"]
                        .as_array()?
                        .iter()
                        .enumerate()
                        .find_map(|(hi, _)| {
                            (id(&source.path, &old_event, gi, hi) == wanted).then_some((gi, hi))
                        })
                })
                .context("Hook no longer exists")?;
            let old_group = groups[gi].clone();
            let hooks = groups[gi]["hooks"].as_array_mut().unwrap();
            let mut new_event = old_event.clone();
            let mut moved = None;
            match action {
                "toggle" => {
                    ensure!(
                        !row["groupDisabled"].as_bool().unwrap_or(false),
                        "This hook group is disabled in its configuration file"
                    );
                    hooks[hi]["disabled"] =
                        json!(!input["enabled"].as_bool().context("Choose enabled state")?);
                }
                "delete" => {
                    hooks.remove(hi);
                }
                _ => {
                    let (event, matcher, handler) = validated(input, Some(&hooks[hi]))?;
                    if event == old_event && matcher == row["matcher"] {
                        hooks[hi] = handler;
                    } else {
                        hooks.remove(hi);
                        let mut group = old_group;
                        group["matcher"] = json!(matcher);
                        group["hooks"] = json!([handler]);
                        moved = Some(group);
                        new_event = event;
                    }
                }
            }
            if hooks.is_empty() {
                groups.remove(gi);
            }
            if let Some(group) = moved {
                events
                    .entry(new_event.clone())
                    .or_insert(json!([]))
                    .as_array_mut()
                    .context("Invalid destination hook event")?
                    .push(group);
            }
            let mut updates = vec![(old_event.clone(), Some(events[&old_event].clone()))];
            if new_event != old_event {
                updates.push((new_event.clone(), Some(events[&new_event].clone())));
            }
            jsonconfig::write_entries(&source.path, "hooks", updates)?;
            return Ok(json!({"saved":true}));
        }
    }
    bail!("Hook no longer exists. Refresh the list.")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_hook_edits_preserve_siblings_and_reject_stale_revisions() {
        let temp = std::env::temp_dir().join(format!("haarness-hooks-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&temp).unwrap();
        let temp = temp.canonicalize().unwrap();
        let workspace = temp.join("project");
        fs::create_dir_all(&workspace).unwrap();
        let paths = Paths::new(Some(&temp.join("home"))).unwrap();
        let path = workspace.join("haarness.jsonc");
        fs::write(
            &path,
            r#"{
          // Preserve my configuration comment.
          "model": "echo", "hooks": {"Stop": [{"matcher":"*", "customGroup":42,
            "hooks":[{"type":"command","command":"echo first","custom":true},
                     {"type":"command","command":"echo sibling"}]}]}}
        "#,
        )
        .unwrap();
        let list = || execute(&paths, &workspace, true, &json!({"action":"list"})).unwrap();
        let row = list()["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["handler"]["command"] == "echo first")
            .unwrap()
            .clone();
        execute(&paths,&workspace,true,&json!({"action":"update","id":row["id"],"revision":row["revision"],"event":"PreToolUse","matcher":"Bash","command":"echo edited","timeout":15})).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("Preserve my configuration comment"));
        let config = jsonconfig::read_json(&path).unwrap().unwrap();
        assert_eq!(config["model"], "echo");
        assert_eq!(
            config["hooks"]["Stop"][0]["hooks"][0]["command"],
            "echo sibling"
        );
        assert_eq!(config["hooks"]["PreToolUse"][0]["customGroup"], 42);
        assert_eq!(config["hooks"]["PreToolUse"][0]["hooks"][0]["custom"], true);
        assert!(
            execute(
                &paths,
                &workspace,
                true,
                &json!({"action":"delete","id":row["id"],"revision":row["revision"]})
            )
            .unwrap_err()
            .to_string()
            .contains("changed")
        );
        let row = list()["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["event"] == "PreToolUse")
            .unwrap()
            .clone();
        execute(
            &paths,
            &workspace,
            true,
            &json!({"action":"toggle","id":row["id"],"revision":row["revision"],"enabled":false}),
        )
        .unwrap();
        assert!(
            list()["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["handler"]["disabled"] == true)
        );
        let untrusted = execute(&paths, &workspace, false, &json!({"action":"list"})).unwrap();
        assert!(
            untrusted["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["source"]["trusted"] == false)
        );
        let row = list()["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["event"] == "PreToolUse")
            .unwrap()
            .clone();
        execute(
            &paths,
            &workspace,
            true,
            &json!({"action":"delete","id":row["id"],"revision":row["revision"]}),
        )
        .unwrap();
        assert_eq!(list()["items"].as_array().unwrap().len(), 1);
        execute(&paths,&workspace,true,&json!({"action":"create","scope":"global","event":"Stop","command":"echo global","timeout":60})).unwrap();
        assert!(
            list()["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["source"]["scope"] == "global")
        );
        fs::remove_dir_all(temp).unwrap();
    }
    #[test]
    fn desktop_hook_validation_and_plugin_readonly() {
        assert!(
            validated(
                &json!({"event":"SubagentStop","command":"echo no","timeout":60}),
                None
            )
            .is_err()
        );
        for event in [
            "SessionStart",
            "SessionEnd",
            "TurnEnd",
            "Notification",
            "FileChanged",
            "ConfigChange",
        ] {
            assert!(
                validated(
                    &json!({"event":event,"command":"echo ok","timeout":60}),
                    None
                )
                .is_ok(),
                "{event}"
            );
        }
        for timeout in [0, 601] {
            assert!(
                validated(
                    &json!({"event":"Stop","command":"echo no","timeout":timeout}),
                    None
                )
                .is_err()
            );
        }
        let path = std::env::temp_dir().join(format!(
            "haarness-hook-plugin-{}.json",
            uuid::Uuid::now_v7()
        ));
        fs::write(&path, "{}").unwrap();
        let source = jsonconfig::HookSource {
            path: path.clone(),
            plugin: true,
            trusted: true,
            block: json!({"Stop":[{"hooks":[{"command":"echo plugin"}]}]}),
            base: false,
        };
        let row = rows(&source, Path::new("/project")).unwrap().remove(0);
        assert_eq!(row["source"]["editable"], false);
        assert_eq!(row["source"]["scope"], "plugin");
        let base = jsonconfig::HookSource {
            plugin: false,
            base: true,
            ..source
        };
        let row = rows(&base, Path::new("/project")).unwrap().remove(0);
        assert_eq!(row["source"]["editable"], false);
        assert_eq!(row["source"]["scope"], "base");
        fs::remove_file(path).unwrap();
    }
}
