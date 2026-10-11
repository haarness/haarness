//! Shared, non-executable client preferences. Read at submission time so edits
//! take effect during active work, independently of runtime asset reloads.
use anyhow::{Context, Result, bail, ensure};
use haarness_protocol::{InputDelivery, ManagementEntry, ManagementField, ManagementView};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(super) fn files(root: &Path) -> Vec<PathBuf> {
    vec![root.join("haarness.jsonc"), root.join("haarness.json")]
}
pub(super) fn project_files(workspace: &Path) -> Vec<PathBuf> {
    std::iter::once(workspace.join(".haarness/cloud/haarness.json"))
        .chain(files(workspace))
        .chain(files(&workspace.join(".haarness")))
        .collect()
}
pub(super) fn layers(global: &Path, workspace: &Path) -> Vec<PathBuf> {
    files(global)
        .into_iter()
        .chain(
            crate::layout::project_directories(workspace)
                .iter()
                .flat_map(|dir| project_files(dir)),
        )
        .collect()
}
fn delivery(path: &Path) -> Result<Option<InputDelivery>> {
    let value = crate::jsonconfig::read_json(path).map_err(anyhow::Error::msg)?;
    let Some(input) = value.as_ref().and_then(|v| v.get("input")) else {
        return Ok(None);
    };
    ensure!(
        input.is_object(),
        "input must be an object in {}",
        path.display()
    );
    input
        .get("delivery")
        .map(|v| {
            serde_json::from_value(v.clone()).with_context(|| {
                format!(
                    "input.delivery must be steer or queue in {}",
                    path.display()
                )
            })
        })
        .transpose()
}
pub(super) fn target(global: &Path, workspace: &Path, scope: &str) -> PathBuf {
    let root = if scope == "global" {
        global.to_path_buf()
    } else {
        workspace.join(".haarness")
    };
    files(&root)
        .into_iter()
        .rev()
        .find(|p| p.exists())
        .unwrap_or_else(|| root.join("haarness.json"))
}
fn snapshot(project_config: bool, global: &Path, workspace: &Path, scope: &str) -> Result<Value> {
    let owned = if scope == "global" {
        files(global)
    } else {
        project_files(workspace)
    };
    let mut effective = InputDelivery::Steer;
    let mut inherited = InputDelivery::Steer;
    let mut selected = None;
    let mut source = "default";
    let mut source_file = None;
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
        if let Some(mode) = delivery(&path)? {
            effective = mode;
            source = if files(global).contains(&path) {
                "global"
            } else {
                "project"
            };
            source_file = Some(path.clone());
            if owned.contains(&path) {
                selected = Some(mode);
            }
            // Global inherits the built-in default; Project inherits all
            // ancestors and global layers, excluding this workspace's files.
            if scope == "project" && !owned.contains(&path) {
                inherited = mode;
            }
        }
    }
    Ok(
        json!({"delivery":effective,"scope":scope,"scopeDelivery":selected,
        "inheritedDelivery":inherited,"source":source,"sourceFile":source_file,
        "file":target(global,workspace,scope),"revision":format!("{:x}",hash.finalize())}),
    )
}

pub(super) fn request(
    project_config: bool,
    global: &Path,
    workspace: &Path,
    input: &Value,
) -> Result<Value> {
    let scope = input["scope"].as_str().unwrap_or("project");
    ensure!(
        matches!(scope, "project" | "global"),
        "Scope must be project or global"
    );
    let action = input["action"].as_str().unwrap_or("get");
    ensure!(
        matches!(
            action,
            "get" | "view" | "set" | "toggle" | "reset" | "command"
        ),
        "Unknown input settings action"
    );
    let command = if action == "command" {
        let args: Vec<_> = input["arguments"]
            .as_str()
            .unwrap_or("")
            .split_whitespace()
            .collect();
        ensure!(
            args.len() <= 1,
            "Usage: /input-mode [steer|queue|toggle|reset]"
        );
        let operation = args.first().copied().unwrap_or("toggle");
        ensure!(
            matches!(operation, "steer" | "queue" | "toggle" | "reset"),
            "Usage: /input-mode [steer|queue|toggle|reset]"
        );
        operation
    } else {
        action
    };
    ensure!(
        matches!(
            command,
            "get" | "view" | "set" | "steer" | "queue" | "toggle" | "reset"
        ),
        "Usage: /input-mode [steer|queue|toggle|reset]"
    );
    // A single installation lock serializes read/modify/write toggles from
    // multiple daemons and clients. The JSON writer separately protects fields.
    let _lock = if matches!(command, "get" | "view") {
        None
    } else {
        fs::create_dir_all(global)?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(global.join("input-settings.lock"))?;
        fs2::FileExt::lock_exclusive(&lock)?;
        Some(lock)
    };
    ensure!(
        project_config || scope == "global" || matches!(command, "get" | "view"),
        "Project configuration is disabled for this server. Choose Global scope to change this setting."
    );
    let before = snapshot(project_config, global, workspace, scope)?;
    let mut message = None;
    if !matches!(command, "get" | "view") {
        if let Some(revision) = input["revision"].as_str() {
            ensure!(
                before["revision"] == revision,
                "Input settings changed. Refresh before saving."
            );
        }
        let current = if scope == "global" {
            before["scopeDelivery"].as_str().unwrap_or("steer")
        } else {
            before["delivery"].as_str().unwrap_or("steer")
        };
        let value = match command {
            "toggle" => Some(if current == "steer" { "queue" } else { "steer" }),
            "steer" => Some("steer"),
            "queue" => Some("queue"),
            "reset" => None,
            "set" => match input["delivery"].as_str() {
                Some("steer") => Some("steer"),
                Some("queue") => Some("queue"),
                Some("inherit") => None,
                _ => bail!("Input delivery must be steer, queue, or inherit"),
            },
            _ => unreachable!(),
        };
        if let Some(value) = value {
            crate::jsonconfig::write_entry(
                &target(global, workspace, scope),
                "input",
                "delivery",
                Some(json!(value)),
            )?;
        } else {
            let owned = if scope == "global" {
                files(global)
            } else {
                project_files(workspace)
            };
            for path in owned {
                if delivery(&path)?.is_some() {
                    crate::jsonconfig::write_entry(&path, "input", "delivery", None)?;
                }
            }
        }
        let after = snapshot(project_config, global, workspace, scope)?;
        let displayed = if scope == "global" {
            after["scopeDelivery"].as_str().unwrap_or("steer")
        } else {
            after["delivery"].as_str().unwrap_or("steer")
        };
        message = Some(format!(
            "Input mode: {} · {} {}",
            displayed,
            if value.is_some() {
                "saved for"
            } else {
                "using inherited setting for"
            },
            if scope == "project" {
                "this project"
            } else {
                "global default"
            }
        ));
    }
    let mut result = snapshot(project_config, global, workspace, scope)?;
    result["message"] = json!(message);
    if input["format"] == "view" || action == "view" {
        return Ok(serde_json::to_value(ManagementView {
            title: "General · Input mode".into(),
            data: Some(result.clone()),
            lines: vec![
                message.unwrap_or_else(|| {
                    format!(
                        "Current mode: {} · {}",
                        result["delivery"].as_str().unwrap(),
                        result["source"].as_str().unwrap()
                    )
                }),
                "Steer joins the next agent step. Queue waits until current work finishes.".into(),
            ],
            fields: vec![
                ManagementField {
                    key: "scope".into(),
                    label: "Scope".into(),
                    value: scope.into(),
                    required: true,
                    secret: false,
                    choices: vec!["project".into(), "global".into()],
                },
                ManagementField {
                    key: "delivery".into(),
                    label: "Messages sent while working".into(),
                    value: result["scopeDelivery"].as_str().unwrap_or("inherit").into(),
                    required: true,
                    secret: false,
                    choices: vec!["inherit".into(), "steer".into(), "queue".into()],
                },
            ],
            submit: Some(json!({"action":"set","format":"view","revision":result["revision"]})),
            entries: vec![ManagementEntry {
                label: "Refresh".into(),
                description: "Read current settings".into(),
                request: json!({"action":"view","scope":scope}),
                confirm: None,
            }],
            ..ManagementView::default()
        })?);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        root: PathBuf,
        global: PathBuf,
        project: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("haarness-input-settings-{}", uuid::Uuid::now_v7()));
            let global = root.join("global");
            let project = root.join("project");
            fs::create_dir_all(project.join(".git")).unwrap();
            fs::create_dir_all(&global).unwrap();
            Self {
                root,
                global,
                project,
            }
        }
        fn request(&self, input: Value) -> Value {
            request(true, &self.global, &self.project, &input).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn input_settings_precedence_and_reset_preserve_ancestors_and_comments() {
        let f = Fixture::new();
        assert_eq!(f.request(json!({"action":"get"}))["delivery"], "steer");
        fs::write(
            f.global.join("haarness.jsonc"),
            "{\n// global comment\n\"input\":{\"delivery\":\"queue\"},\"unrelated\":42\n}",
        )
        .unwrap();
        assert_eq!(f.request(json!({"action":"get"}))["delivery"], "queue");
        fs::write(
            f.project.join("haarness.json"),
            r#"{"input":{"delivery":"steer"}}"#,
        )
        .unwrap();
        let child = f.project.join("child");
        fs::create_dir_all(child.join(".haarness")).unwrap();
        let path = child.join(".haarness/haarness.jsonc");
        fs::write(
            &path,
            "{\n// keep child comment\n\"input\":{\"other\":true},\"unrelated\":123\n}",
        )
        .unwrap();
        let before = fs::read(f.project.join("haarness.json")).unwrap();
        let result = request(true, &f.global, &child, &json!({"action":"toggle"})).unwrap();
        assert_eq!(result["delivery"], "queue");
        assert_eq!(result["file"], json!(path));
        assert_eq!(fs::read(f.project.join("haarness.json")).unwrap(), before);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("// keep child comment") && text.contains("123"));
        assert_eq!(
            request(true, &f.global, &child, &json!({"action":"reset"})).unwrap()["delivery"],
            "steer"
        );
        assert_eq!(fs::read(f.project.join("haarness.json")).unwrap(), before);
        // Reset all direct overrides, never an inherited ancestor.
        fs::write(
            child.join("haarness.json"),
            r#"{"input":{"delivery":"queue"}}"#,
        )
        .unwrap();
        request(true, &f.global, &child, &json!({"action":"toggle"})).unwrap();
        let result = request(true, &f.global, &child, &json!({"action":"reset"})).unwrap();
        assert_eq!(result["scopeDelivery"], Value::Null);
        assert_eq!(result["delivery"], "steer");
    }
    #[test]
    fn input_settings_global_edit_json_priority_and_stale_revision() {
        let f = Fixture::new();
        f.request(json!({"action":"set","scope":"global","delivery":"queue"}));
        f.request(json!({"action":"set","delivery":"steer"}));
        let old = f.request(json!({"action":"get"}));
        // Global updates do not disturb an explicit project preference.
        let changed = f.request(json!({"action":"set","scope":"global","delivery":"steer"}));
        assert_eq!(changed["delivery"], "steer");
        assert!(
            request(
                true,
                &f.global,
                &f.project,
                &json!({"action":"set","delivery":"queue","revision":old["revision"]})
            )
            .is_err()
        );
        fs::write(
            f.project.join(".haarness/haarness.jsonc"),
            r#"{"input":{"delivery":"queue"}}"#,
        )
        .unwrap();
        assert_eq!(f.request(json!({"action":"get"}))["delivery"], "steer"); // JSON wins.
        let reset = f.request(json!({"action":"reset"}));
        assert_eq!(reset["scopeDelivery"], Value::Null);
        assert_eq!(reset["delivery"], "steer");
        for args in ["bad", "queue extra", "input-mode", "get", "view", "set"] {
            assert!(
                request(
                    true,
                    &f.global,
                    &f.project,
                    &json!({"action":"command","arguments":args})
                )
                .is_err()
            );
        }
        fs::write(
            f.project.join("haarness.json"),
            r#"{"input":{"delivery":"bad"}}"#,
        )
        .unwrap();
        assert!(request(true, &f.global, &f.project, &json!({"action":"get"})).is_err());
    }
    #[test]
    fn input_settings_concurrent_toggles_are_serialized() {
        let f = Fixture::new();
        std::thread::scope(|s| {
            for _ in 0..10 {
                s.spawn(|| {
                    f.request(json!({"action":"toggle"}));
                });
            }
        });
        assert_eq!(f.request(json!({"action":"get"}))["delivery"], "steer");
        f.request(json!({"action":"command","arguments":""}));
        assert_eq!(f.request(json!({"action":"get"}))["delivery"], "queue");
    }
}
