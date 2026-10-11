//! Live, revision-checked browser settings shared by Desktop, the TUI and
//! attached clients: `browser.policy` for the Haarness browser, layered as
//! every other setting, and `browser.chrome` for the user's own Chrome, which
//! only the global layer sets (a project may only tighten it).
use crate::input_settings::{files, layers, project_files, target};
use anyhow::{Result, bail, ensure};
use haarness_server::browser_policy::{
    BrowserPolicy, BrowserSettings, ChromeLayering, ChromeRestriction, ChromeSettings,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

const CHROME_SCOPE: &str =
    "Chrome settings apply to every project on this computer; save them globally.";

/// Where `haarness browser setup` writes the native-messaging wrapper Chrome
/// starts, so the broker can tell "not set up" from "not connected".
pub(super) fn chrome_wrapper(data: &Path) -> PathBuf {
    data.join("browser").join("chrome").join(if cfg!(windows) {
        "haarness-browser-host.bat"
    } else {
        "haarness-browser-host"
    })
}

/// One layer's `browser.policy` (strict, as before) and raw `browser.chrome`
/// (read leniently later, so it can never break the Haarness browser).
fn read(path: &Path) -> Result<(Option<BrowserPolicy>, Option<Value>)> {
    let raw = crate::jsonconfig::read_json(path).map_err(anyhow::Error::msg)?;
    let browser = raw.as_ref().and_then(|value| value.get("browser"));
    let policy = match browser.and_then(|value| value.get("policy")) {
        Some(value) => {
            let mut policy: BrowserPolicy = serde_json::from_value(value.clone())?;
            policy.validate()?;
            Some(policy)
        }
        None => None,
    };
    let chrome = browser
        .and_then(|value| value.get("chrome"))
        .filter(|value| !value.is_null())
        .cloned();
    Ok((policy, chrome))
}
struct Snapshot {
    value: Value,
    settings: BrowserSettings,
}
fn snapshot(
    project_config: bool,
    global: &Path,
    workspace: &Path,
    scope: &str,
) -> Result<Snapshot> {
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
    let mut shared = Vec::new();
    let mut global_chrome = None;
    let mut project_chrome = Vec::new();
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
        let is_global = files(global).contains(&path);
        let (policy, chrome) = read(&path)?;
        if let Some(chrome) = chrome {
            if is_global {
                global_chrome = Some(chrome);
            } else {
                project_chrome.push(chrome);
            }
        }
        if let Some(policy) = policy {
            shared.push(policy.clone());
            effective = policy.clone();
            source = if is_global { "global" } else { "project" };
            if owned.contains(&path) {
                selected = Some(policy.clone());
            }
            if scope == "project" && !owned.contains(&path) {
                inherited = policy;
            }
        }
    }
    let chrome = ChromeLayering::new(global_chrome.as_ref(), &project_chrome, &shared);
    let global_settings = global_chrome.as_ref().and_then(ChromeSettings::lenient);
    Ok(Snapshot {
        value: json!({
            "policy": effective,
            "scopePolicy": selected,
            "inheritedPolicy": inherited,
            "source": source,
            "scope": scope,
            "revision": format!("{:x}", hash.finalize()),
            "chrome": {
                "settings": chrome.settings,
                "globalSettings": global_settings,
                // In English, for clients older than `restrictions`.
                "projectRestrictions": chrome
                    .restrictions
                    .iter()
                    .map(ChromeRestriction::describe)
                    .collect::<Vec<_>>(),
                "restrictions": chrome.restrictions,
            },
        }),
        settings: BrowserSettings {
            default_browser: chrome.settings.default_browser,
            policy: effective,
            chrome: chrome.policy,
            chrome_switched_off: !chrome.global_enabled,
            chrome_blocked_by: chrome.blocked_by,
        },
    })
}
pub(super) fn load(
    project_config: bool,
    global: &Path,
    workspace: &Path,
) -> Result<BrowserSettings> {
    Ok(snapshot(project_config, global, workspace, "project")?.settings)
}
/// Which settings a reset clears. Older clients send no parts and reset only
/// the Haarness browser's settings.
fn reset_parts(input: &Value) -> Result<(bool, bool)> {
    let Some(parts) = input.get("parts").filter(|parts| !parts.is_null()) else {
        return Ok((true, false));
    };
    let Some(parts) = parts.as_array() else {
        bail!("Reset parts must be a list");
    };
    let mut policy = false;
    let mut chrome = false;
    for part in parts {
        match part.as_str() {
            Some("policy") => policy = true,
            Some("chrome") => chrome = true,
            _ => bail!("Reset parts are policy and chrome"),
        }
    }
    Ok((policy, chrome))
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
                .is_some_and(|revision| before.value["revision"] == revision),
            "Browser settings changed. Refresh before saving."
        );
        if action == "set" {
            let policy = input.get("policy").filter(|value| !value.is_null());
            let chrome = input.get("chrome").filter(|value| !value.is_null());
            ensure!(
                policy.is_some() || chrome.is_some(),
                "Choose browser settings to save"
            );
            ensure!(chrome.is_none() || scope == "global", CHROME_SCOPE);
            // Check everything before writing anything.
            let policy = match policy {
                Some(value) => {
                    let mut policy: BrowserPolicy = serde_json::from_value(value.clone())?;
                    policy.validate()?;
                    Some(policy)
                }
                None => None,
            };
            let chrome = chrome.map(ChromeSettings::strict).transpose()?;
            if let Some(policy) = policy {
                crate::jsonconfig::write_entry(
                    &target(global, workspace, scope),
                    "browser",
                    "policy",
                    Some(serde_json::to_value(policy)?),
                )?;
            }
            if let Some(chrome) = chrome {
                crate::jsonconfig::write_entry(
                    &target(global, workspace, "global"),
                    "browser",
                    "chrome",
                    Some(serde_json::to_value(chrome)?),
                )?;
            }
        } else {
            let (policy, chrome) = reset_parts(input)?;
            ensure!(!chrome || scope == "global", CHROME_SCOPE);
            if policy {
                for path in if scope == "global" {
                    files(global)
                } else {
                    project_files(workspace)
                } {
                    if read(&path)?.0.is_some() {
                        crate::jsonconfig::write_entry(&path, "browser", "policy", None)?;
                    }
                }
            }
            if chrome {
                for path in files(global) {
                    if read(&path)?.1.is_some() {
                        crate::jsonconfig::write_entry(&path, "browser", "chrome", None)?;
                    }
                }
            }
        }
    }
    Ok(snapshot(project_config, global, workspace, scope)?.value)
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
        let mut policy = BrowserPolicy {
            enabled: false,
            ..BrowserPolicy::default()
        };
        let disabled = request(
            true,
            &global,
            &workspace,
            &json!({"scope":"global","action":"set","revision":first["revision"],"policy":policy}),
        )
        .unwrap();
        assert!(!load(true, &global, &workspace).unwrap().policy.enabled);
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
        assert!(load(true, &global, &workspace).unwrap().policy.enabled);
        request(
            true,
            &global,
            &workspace,
            &json!({"action":"reset","revision":enabled["revision"]}),
        )
        .unwrap();
        assert!(!load(true, &global, &workspace).unwrap().policy.enabled);
        assert!(
            fs::read_to_string(workspace.join("haarness.jsonc"))
                .unwrap()
                .contains("keep this")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    /// A scratch directory removed when the test ends.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            Self(
                std::env::temp_dir()
                    .join(format!("haarness-chrome-settings-{}", uuid::Uuid::now_v7())),
            )
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn complete(enabled: bool) -> Value {
        json!({
            "enabled": enabled,
            "defaults": {"browsing":"ask","downloads":"ask","uploads":"deny","scripts":"ask"},
            "defaultBrowser": "chrome",
            "sites": [{"origin":"https://Example.com:443/","browsing":"allow","downloads":"ask","uploads":"ask","scripts":"ask"}],
        })
    }

    #[test]
    fn chrome_settings_save_globally_reset_by_part_and_reject_project_scope() {
        let dir = Scratch::new();
        let global = dir.path().join("global");
        let workspace = dir.path().join("project");
        fs::create_dir_all(&workspace).unwrap();
        let first = request(true, &global, &workspace, &json!({})).unwrap();
        assert_eq!(first["chrome"]["globalSettings"], Value::Null);
        assert_eq!(first["chrome"]["settings"]["enabled"], true);
        assert_eq!(first["chrome"]["settings"]["defaults"]["browsing"], "ask");
        assert_eq!(first["chrome"]["settings"]["defaultBrowser"], "auto");
        // A project may not change Chrome, even when it asks to.
        let error = request(
            true,
            &global,
            &workspace,
            &json!({"scope":"project","action":"set","revision":first["revision"],"chrome":complete(true)}),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), CHROME_SCOPE);
        // Settings are complete or refused.
        assert!(
            request(
                true,
                &global,
                &workspace,
                &json!({"scope":"global","action":"set","revision":first["revision"],"chrome":{"enabled":false}}),
            )
            .is_err()
        );
        let saved = request(
            true,
            &global,
            &workspace,
            &json!({"scope":"global","action":"set","revision":first["revision"],"chrome":complete(true)}),
        )
        .unwrap();
        assert_eq!(
            saved["chrome"]["globalSettings"]["sites"][0]["origin"],
            "https://example.com"
        );
        assert_eq!(saved["chrome"]["settings"]["defaultBrowser"], "chrome");
        assert!(
            fs::read_to_string(global.join("haarness.json"))
                .unwrap()
                .contains("\"chrome\"")
        );
        let settings = load(true, &global, &workspace).unwrap();
        assert_eq!(
            settings.chrome.decision(
                haarness_server::browser_policy::ChromeCategory::Browsing,
                "https://example.com/inbox"
            ),
            haarness_core::PermissionDecision::Allow
        );
        // An older Desktop's reset leaves Chrome alone.
        let policy = request(
            true,
            &global,
            &workspace,
            &json!({"scope":"global","action":"set","revision":saved["revision"],"policy":BrowserPolicy::default()}),
        )
        .unwrap();
        let reset = request(
            true,
            &global,
            &workspace,
            &json!({"scope":"global","action":"reset","revision":policy["revision"]}),
        )
        .unwrap();
        assert_eq!(reset["source"], "default");
        assert_eq!(reset["chrome"]["globalSettings"]["enabled"], true);
        let cleared = request(
            true,
            &global,
            &workspace,
            &json!({"scope":"global","action":"reset","revision":reset["revision"],"parts":["chrome"]}),
        )
        .unwrap();
        assert_eq!(cleared["chrome"]["globalSettings"], Value::Null);
        assert!(
            request(
                true,
                &global,
                &workspace,
                &json!({"scope":"project","action":"reset","revision":cleared["revision"],"parts":["chrome"]}),
            )
            .is_err()
        );
    }

    #[test]
    fn chrome_settings_are_global_project_layers_only_tighten_and_bad_values_never_break_desktop() {
        use haarness_core::PermissionDecision::{Allow, Ask, Deny};
        use haarness_server::browser_policy::{ChromeBlock, ChromeCategory as C, DefaultBrowser};
        let dir = Scratch::new();
        let global = dir.path().join("global");
        let workspace = dir.path().join("project");
        fs::create_dir_all(&global).unwrap();
        fs::create_dir_all(workspace.join(".haarness")).unwrap();
        fs::write(
            global.join("haarness.json"),
            json!({"browser":{
                "chrome":{"enabled":true,"defaults":{"browsing":"allow","downloads":"ask","uploads":"ask","scripts":"surprise"},"defaultBrowser":"firefox","sites":[{"origin":"https://mail.example.com","browsing":"allow","downloads":"allow","uploads":"allow","scripts":"allow"},{"origin":"not a site","browsing":"allow"}],"later":true},
                "policy":{"enabled":true,"defaults":{"browsing":"allow","downloads":"allow","uploads":"allow"},"sites":[{"origin":"https://blocked.example.com","browsing":"deny","downloads":"allow","uploads":"allow"}]}
            }})
            .to_string(),
        )
        .unwrap();
        // The project tries to loosen (ignored) and tightens (applied).
        fs::write(
            workspace.join(".haarness/haarness.json"),
            json!({"browser":{
                "chrome":{"defaults":{"browsing":"allow","uploads":"deny"},"defaultBrowser":"haarness","sites":[{"origin":"https://mail.example.com","scripts":"deny"},{"origin":"https://open.example.com","browsing":"allow"}]},
                "policy":{"enabled":true,"defaults":{"browsing":"allow","downloads":"allow","uploads":"allow"},"sites":[{"origin":"https://blocked.example.com","browsing":"allow","downloads":"allow","uploads":"allow"}]}
            }})
            .to_string(),
        )
        .unwrap();
        let settings = load(true, &global, &workspace).unwrap();
        // Desktop's policy still reads: the project's layer wins there.
        assert_eq!(
            settings
                .policy
                .decision("navigate", "https://blocked.example.com"),
            Allow
        );
        let chrome = &settings.chrome;
        assert_eq!(settings.default_browser, DefaultBrowser::Auto);
        assert_eq!(
            chrome.decision(C::Browsing, "https://news.example.org"),
            Allow
        );
        assert_eq!(chrome.decision(C::Scripts, "https://news.example.org"), Ask);
        assert_eq!(
            chrome.decision(C::Uploads, "https://news.example.org"),
            Deny
        );
        assert_eq!(
            chrome.decision(C::Browsing, "https://mail.example.com"),
            Allow
        );
        assert_eq!(
            chrome.decision(C::Scripts, "https://mail.example.com"),
            Deny
        );
        // The project's stricter default reaches sites it says nothing else about.
        assert_eq!(
            chrome.decision(C::Uploads, "https://mail.example.com"),
            Deny
        );
        // The global layer's shared block applies; the project's allow does not.
        assert_eq!(
            chrome.decision(C::Browsing, "https://blocked.example.com"),
            Deny
        );
        assert_eq!(
            chrome.decision(C::Scripts, "https://blocked.example.com"),
            Deny
        );
        let value = request(true, &global, &workspace, &json!({})).unwrap();
        let restrictions = value["chrome"]["projectRestrictions"].as_array().unwrap();
        assert!(restrictions.contains(&json!("Uploads: Block by default")));
        assert!(restrictions.contains(&json!("Scripts on https://mail.example.com: Block")));
        assert_eq!(restrictions.len(), 2, "{restrictions:?}");
        // The same, as data clients put into their own words.
        let structured = value["chrome"]["restrictions"].as_array().unwrap();
        assert!(
            structured.contains(&json!({"kind":"default","category":"uploads","decision":"deny"}))
        );
        assert!(structured.contains(&json!({"kind":"site","origin":"https://mail.example.com","category":"scripts","decision":"deny"})));
        assert_eq!(structured.len(), 2, "{structured:?}");
        assert_eq!(
            value["chrome"]["globalSettings"]["defaults"]["scripts"],
            "ask"
        );
        assert_eq!(
            value["chrome"]["globalSettings"]["sites"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        // A project turns Chrome off; an untrusted project cannot.
        fs::write(
            workspace.join("haarness.json"),
            json!({"browser":{"chrome":{"enabled":false}}}).to_string(),
        )
        .unwrap();
        let settings = load(true, &global, &workspace).unwrap();
        assert!(!settings.chrome.enabled && !settings.chrome_switched_off);
        assert_eq!(settings.chrome_blocked_by, Some(ChromeBlock::Project));
        assert_eq!(
            request(true, &global, &workspace, &json!({})).unwrap()["chrome"]["restrictions"][0],
            json!({"kind":"off"})
        );
        let settings = load(false, &global, &workspace).unwrap();
        assert!(settings.chrome.enabled && settings.chrome_blocked_by.is_none());
        // Unreadable Chrome settings fall back to Chrome's defaults; the
        // Haarness browser keeps working.
        fs::write(
            global.join("haarness.json"),
            json!({"browser":{"chrome":{"enabled":"yes","sites":7},"policy":{"enabled":true,"defaults":{"browsing":"allow","downloads":"allow","uploads":"allow"},"sites":[]}}}).to_string(),
        )
        .unwrap();
        let settings = load(false, &global, &workspace).unwrap();
        assert!(settings.policy.enabled);
        assert!(settings.chrome.enabled);
        assert_eq!(
            settings.chrome.decision(C::Browsing, "https://example.com"),
            Ask
        );
        // Turning agent browser control off turns Chrome off too.
        fs::write(
            global.join("haarness.json"),
            json!({"browser":{"policy":{"enabled":false,"defaults":{"browsing":"allow","downloads":"allow","uploads":"allow"},"sites":[]}}}).to_string(),
        )
        .unwrap();
        let settings = load(false, &global, &workspace).unwrap();
        assert!(!settings.chrome.enabled && !settings.chrome_switched_off);
        assert_eq!(
            settings.chrome_blocked_by,
            Some(ChromeBlock::BrowserControl)
        );
        // Chrome's own switch off is its own reason.
        fs::write(
            global.join("haarness.json"),
            json!({"browser":{"chrome":{"enabled":false}}}).to_string(),
        )
        .unwrap();
        let settings = load(false, &global, &workspace).unwrap();
        assert!(!settings.chrome.enabled && settings.chrome_switched_off);
        assert_eq!(settings.chrome_blocked_by, None);
    }
}
