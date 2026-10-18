//! Haarness `plugin`, `plugins` and `extensions` configuration keys.
//!
//! - `extensions: [spec | [spec, options]]` and path/npm entries of
//!   `plugin: [...]` name JavaScript extensions. They register
//!   through the same path as `extensions.json` (`extensions::sources`): file
//!   specs resolve relative to the declaring file, npm specs resolve to a
//!   package already installed by `haarness extension install npm:<spec>`.
//!   Nothing is downloaded while configuration loads.
//! - `plugins: [spec | {spec, enabled}]` and `name@marketplace`
//!   entries of `plugin` name marketplace plugins. Nothing is cloned while
//!   configuration loads: plugins that are not installed are reported as
//!   pending installs (startup notice, `haarness plugin list`), and
//!   `enabled: false` hides an installed plugin from discovery.
use crate::{Paths, jsonconfig, layout, plugins};
use anyhow::{Context as _, Result, bail};
use haarness_extensions::Entry;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionSpec {
    pub spec: String,
    pub options: Value,
    /// The configuration file that declared it; relative paths resolve here.
    pub file: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginSpec {
    pub spec: String,
    pub enabled: bool,
    pub file: PathBuf,
}

#[derive(Debug, Default)]
pub struct Declarations {
    pub extensions: Vec<ExtensionSpec>,
    pub plugins: Vec<PluginSpec>,
    /// Entries that could not be understood, as human-readable notices.
    pub notices: Vec<String>,
}

pub enum Resolution {
    Entry(Entry),
    /// A registry package that `haarness extension install` has not fetched.
    Pending {
        package: String,
        install: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Extension,
    Plugin,
}

/// The JSON/JSONC files the loader reads, global first, then project layers.
pub fn config_files(config_dir: &Path, workspace: &Path, trust: bool) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = [
        "opencode.jsonc",
        "opencode.json",
        "haarness.jsonc",
        "haarness.json",
    ]
    .iter()
    .map(|name| config_dir.join(name))
    .collect();
    if trust {
        for directory in layout::project_directories(workspace) {
            for root in [
                directory.clone(),
                directory.join(".opencode"),
                directory.join(".haarness"),
            ] {
                for name in [
                    "opencode.jsonc",
                    "opencode.json",
                    "haarness.jsonc",
                    "haarness.json",
                ] {
                    files.push(root.join(name));
                }
            }
        }
    }
    files
}

fn is_path_spec(spec: &str) -> bool {
    spec.starts_with("file://")
        || spec.starts_with("./")
        || spec.starts_with("../")
        || spec.starts_with('/')
        || spec.starts_with("~/")
}

fn looks_like_version(suffix: &str) -> bool {
    suffix.is_empty()
        || suffix.starts_with(|c: char| c.is_ascii_digit() || "^~><=*".contains(c))
        || matches!(
            suffix,
            "latest" | "next" | "beta" | "alpha" | "canary" | "x"
        )
}

/// Whether a `plugin` entry names a JavaScript package/path
/// (an extension) or a `name@marketplace` plugin bundle.
#[must_use]
pub fn classify(spec: &str) -> Kind {
    let spec = spec.trim();
    if is_path_spec(spec) || spec.starts_with("npm:") || spec.starts_with('@') {
        return Kind::Extension;
    }
    match spec.rsplit_once('@') {
        Some((_, suffix)) => {
            if looks_like_version(suffix) {
                Kind::Extension
            } else {
                Kind::Plugin
            }
        }
        None if spec.contains('/') => Kind::Plugin,
        None => Kind::Extension,
    }
}

fn extension_spec(item: &Value) -> Option<(String, Value)> {
    match item {
        Value::String(spec) if !spec.trim().is_empty() => Some((spec.clone(), Value::Null)),
        Value::Array(tuple) => match tuple.as_slice() {
            [Value::String(spec)] if !spec.trim().is_empty() => Some((spec.clone(), Value::Null)),
            [
                Value::String(spec),
                options @ (Value::Object(_) | Value::Null),
            ] if !spec.trim().is_empty() => Some((spec.clone(), options.clone())),
            _ => None,
        },
        _ => None,
    }
}

/// Parse the three keys of one configuration document.
#[must_use]
pub fn parse(value: &Value, file: &Path) -> Declarations {
    let mut result = Declarations::default();
    let label = file.display();
    let file = file.to_path_buf();
    match value.get("extensions") {
        None => {}
        Some(Value::Array(items)) => {
            for (index, item) in items.iter().enumerate() {
                match extension_spec(item) {
                    Some((spec, options)) => result.extensions.push(ExtensionSpec {
                        spec,
                        options,
                        file: file.clone(),
                    }),
                    None => result.notices.push(format!(
                        "{label}: extensions[{index}] must be a spec string or [spec, options]; ignored"
                    )),
                }
            }
        }
        Some(_) => result
            .notices
            .push(format!("{label}: extensions must be an array; ignored")),
    }
    match value.get("plugins") {
        None => {}
        Some(Value::Array(items)) => {
            for (index, item) in items.iter().enumerate() {
                let spec = match item {
                    Value::String(spec) => Some((spec.clone(), true)),
                    Value::Object(object) => {
                        object.get("spec").and_then(Value::as_str).map(|spec| {
                            (
                                spec.to_owned(),
                                object
                                    .get("enabled")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(true),
                            )
                        })
                    }
                    _ => None,
                };
                match spec {
                    Some((spec, enabled)) if !spec.trim().is_empty() => result.plugins.push(PluginSpec {
                        spec,
                        enabled,
                        file: file.clone(),
                    }),
                    _ => result.notices.push(format!(
                        "{label}: plugins[{index}] must be \"name@marketplace\" or {{\"spec\", \"enabled\"}}; ignored"
                    )),
                }
            }
        }
        Some(_) => result
            .notices
            .push(format!("{label}: plugins must be an array; ignored")),
    }
    match value.get("plugin") {
        None => {}
        Some(Value::Array(items)) => {
            for (index, item) in items.iter().enumerate() {
                match item.as_str().map(str::trim).filter(|spec| !spec.is_empty()) {
                    Some(spec) => match classify(spec) {
                        Kind::Extension => result.extensions.push(ExtensionSpec {
                            spec: spec.to_owned(),
                            options: Value::Null,
                            file: file.clone(),
                        }),
                        Kind::Plugin => result.plugins.push(PluginSpec {
                            spec: spec.to_owned(),
                            enabled: true,
                            file: file.clone(),
                        }),
                    },
                    None => result.notices.push(format!(
                        "{label}: plugin[{index}] must be a spec string; ignored"
                    )),
                }
            }
        }
        Some(_) => result
            .notices
            .push(format!("{label}: plugin must be an array; ignored")),
    }
    result
}

/// Loader-side notices for one document (invalid entries only; valid
/// declarations are silent because registration happens elsewhere).
#[must_use]
pub fn describe(value: &Value, file: &Path, trusted: bool) -> Vec<String> {
    let parsed = parse(value, file);
    let mut notices = parsed.notices;
    if !trusted && (!parsed.extensions.is_empty() || !parsed.plugins.is_empty()) {
        notices.push(format!(
            "{}: extension and plugin declarations wait for project trust",
            file.display()
        ));
    }
    notices
}

/// Every declaration across the global and (trusted) project layers.
#[must_use]
pub fn declarations(config_dir: &Path, workspace: &Path, trust: bool) -> Declarations {
    let mut result = Declarations::default();
    for file in config_files(config_dir, workspace, trust) {
        if let Ok(Some(value)) = jsonconfig::read_json(&file) {
            let parsed = parse(&value, &file);
            result.extensions.extend(parsed.extensions);
            result.plugins.extend(parsed.plugins);
            result.notices.extend(parsed.notices);
        }
    }
    result
}

/// `pkg@1.2.3` -> `pkg`; `@scope/pkg@^1` -> `@scope/pkg`.
#[must_use]
pub fn npm_package_name(spec: &str) -> &str {
    match spec.rsplit_once('@') {
        Some((name, _)) if !name.is_empty() => name,
        _ => spec,
    }
}

fn validate_npm_spec(spec: &str) -> Result<()> {
    if spec.is_empty()
        || spec.starts_with('-')
        || !spec
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"@/._-+^~><=*".contains(&byte))
    {
        bail!("invalid npm extension spec {spec:?}");
    }
    let name = npm_package_name(spec);
    if name.contains("..")
        || name.starts_with('/')
        || (name.contains('/') && (!name.starts_with('@') || name.matches('/').count() != 1))
    {
        bail!("invalid npm package name {name:?}");
    }
    Ok(())
}

/// The newest package directory `haarness extension install npm:` created
/// for `name`, if any.
#[must_use]
pub fn installed_npm_package(data: &Path, name: &str) -> Option<PathBuf> {
    let mut roots: Vec<PathBuf> = std::fs::read_dir(data.join("extensions/packages"))
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|root| root.join("node_modules").join(name).is_dir())
        .collect();
    roots.sort();
    roots.pop().map(|root| root.join("node_modules").join(name))
}

/// Map one declared extension onto an `extensions.json`-style entry.
pub fn resolve_extension(spec: &ExtensionSpec, data: &Path) -> Result<Resolution> {
    let text = spec.spec.trim();
    let npm = text
        .strip_prefix("npm:")
        .or_else(|| (!is_path_spec(text)).then_some(text));
    if let Some(package) = npm {
        validate_npm_spec(package)?;
        let name = npm_package_name(package);
        return Ok(match installed_npm_package(data, name) {
            Some(root) => Resolution::Entry(Entry {
                name: None,
                source: haarness_extensions::entrypoint(&root)?,
                options: spec.options.clone(),
                enabled: true,
            }),
            None => Resolution::Pending {
                package: name.to_owned(),
                install: format!("haarness extension install npm:{package}"),
            },
        });
    }
    let raw = text.strip_prefix("file://").unwrap_or(text);
    let path = if let Some(rest) = raw.strip_prefix("~/") {
        haarness_protocol::home_dir()
            .context("the home folder is unknown (set HOME or USERPROFILE)")?
            .join(rest)
    } else if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        spec.file
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(raw)
    };
    Ok(Resolution::Entry(Entry {
        name: None,
        source: haarness_extensions::entrypoint(&path)?,
        options: spec.options.clone(),
        enabled: true,
    }))
}

/// `name@marketplace`, `marketplace/name` or a bare name.
#[must_use]
pub fn plugin_reference(spec: &str) -> (String, Option<String>) {
    let spec = spec.trim();
    if let Some((name, market)) = spec.rsplit_once('@').filter(|(name, _)| !name.is_empty()) {
        return (name.to_owned(), Some(market.to_owned()));
    }
    if let Some((market, name)) = spec.split_once('/') {
        return (name.to_owned(), Some(market.to_owned()));
    }
    (spec.to_owned(), None)
}

/// Declared, enabled plugins that the plugin registry has not installed.
#[must_use]
pub fn pending_plugins(data: &Path, declared: &[PluginSpec]) -> Vec<PluginSpec> {
    let installed = plugins::installed(data);
    declared
        .iter()
        .filter(|plugin| plugin.enabled)
        .filter(|plugin| {
            let (name, market) = plugin_reference(&plugin.spec);
            !installed.get(&name).is_some_and(|record| {
                market
                    .as_deref()
                    .is_none_or(|market| market == record.marketplace)
            })
        })
        .cloned()
        .collect()
}

/// Installed plugin names that configuration declares `enabled: false`.
#[must_use]
pub fn disabled_plugins(config_dir: &Path, workspace: &Path, trust: bool) -> BTreeSet<String> {
    declarations(config_dir, workspace, trust)
        .plugins
        .iter()
        .filter(|plugin| !plugin.enabled)
        .map(|plugin| plugin_reference(&plugin.spec).0)
        .collect()
}

/// What a user must still do for declarations to take effect.
#[must_use]
pub fn startup_notices(paths: &Paths, workspace: &Path, trust: bool) -> Vec<String> {
    let declared = declarations(paths.config_dir(), workspace, trust);
    let mut notices = declared.notices;
    for spec in &declared.extensions {
        match resolve_extension(spec, &paths.data) {
            Ok(Resolution::Entry(_)) => {}
            Ok(Resolution::Pending { package, install }) => notices.push(format!(
                "extension package {package} is declared in {} but not installed; run `{install}`",
                spec.file.display()
            )),
            Err(error) => notices.push(format!(
                "extension {} declared in {} cannot be resolved: {error}",
                spec.spec,
                spec.file.display()
            )),
        }
    }
    for plugin in pending_plugins(&paths.data, &declared.plugins) {
        notices.push(format!(
            "plugin {} is declared in {} but not installed; run `haarness plugin install {}`",
            plugin.spec,
            plugin.file.display(),
            plugin.spec
        ));
    }
    notices
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn scratch(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("haarness-{label}-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn declarations_parse_extension_and_plugin_shapes() {
        let file = Path::new("/cfg/haarness.json");
        let parsed = parse(
            &json!({
                "extensions": ["./ext/index.ts", ["@team/reviewer@1.0.0", {"strict": true}], 42, ["x", "bad"]],
                "plugins": ["reviewer@internal", {"spec": "linter@internal", "enabled": false}, {"enabled": true}],
                "plugin": ["file:///abs/plugin.js", "example-notify", "@scope/pkg@^2", "docs@marketplace", "market/name", "pkg@1.2.3"]
            }),
            file,
        );
        let extensions: Vec<_> = parsed
            .extensions
            .iter()
            .map(|item| item.spec.as_str())
            .collect();
        assert_eq!(
            extensions,
            [
                "./ext/index.ts",
                "@team/reviewer@1.0.0",
                "file:///abs/plugin.js",
                "example-notify",
                "@scope/pkg@^2",
                "pkg@1.2.3"
            ]
        );
        assert_eq!(parsed.extensions[1].options, json!({"strict": true}));
        assert_eq!(parsed.extensions[0].file, file);
        let plugins: Vec<_> = parsed
            .plugins
            .iter()
            .map(|item| (item.spec.as_str(), item.enabled))
            .collect();
        assert_eq!(
            plugins,
            [
                ("reviewer@internal", true),
                ("linter@internal", false),
                ("docs@marketplace", true),
                ("market/name", true)
            ]
        );
        assert_eq!(parsed.notices.len(), 3, "{:?}", parsed.notices);
        assert!(parsed.notices[0].contains("extensions[2]"));
        assert!(parsed.notices[1].contains("extensions[3]"));
        assert!(parsed.notices[2].contains("plugins[2]"));
        assert_eq!(classify("name@latest"), Kind::Extension);
        assert_eq!(classify("name@internal"), Kind::Plugin);
        assert_eq!(
            plugin_reference("linter@internal"),
            ("linter".into(), Some("internal".into()))
        );
        assert_eq!(
            plugin_reference("internal/linter"),
            ("linter".into(), Some("internal".into()))
        );
        assert_eq!(plugin_reference("linter"), ("linter".into(), None));
        assert_eq!(npm_package_name("@scope/pkg@^2"), "@scope/pkg");
        assert_eq!(npm_package_name("pkg"), "pkg");
    }

    #[test]
    fn extension_specs_resolve_to_entries_or_pending_installs() {
        let root = scratch("config-specs");
        let config_dir = root.join("config");
        let data = root.join("data");
        fs::create_dir_all(config_dir.join("ext")).unwrap();
        fs::write(
            config_dir.join("ext/index.ts"),
            "export default {apiVersion:1,name:'local'};",
        )
        .unwrap();
        let file = config_dir.join("haarness.json");
        let local = ExtensionSpec {
            spec: "./ext".into(),
            options: json!({"k":1}),
            file: file.clone(),
        };
        let Resolution::Entry(entry) = resolve_extension(&local, &data).unwrap() else {
            panic!("local path must resolve");
        };
        assert_eq!(
            entry.source,
            config_dir.join("ext/index.ts").canonicalize().unwrap()
        );
        assert_eq!(entry.options, json!({"k":1}));
        assert!(entry.enabled);
        let url = ExtensionSpec {
            spec: format!("file://{}", config_dir.join("ext/index.ts").display()),
            options: Value::Null,
            file: file.clone(),
        };
        assert!(matches!(
            resolve_extension(&url, &data).unwrap(),
            Resolution::Entry(_)
        ));
        let missing = ExtensionSpec {
            spec: "./missing.ts".into(),
            options: Value::Null,
            file: file.clone(),
        };
        assert!(resolve_extension(&missing, &data).is_err());

        let package = ExtensionSpec {
            spec: "@team/reviewer@1.0.0".into(),
            options: json!({"strict":true}),
            file: file.clone(),
        };
        let Resolution::Pending {
            package: name,
            install,
        } = resolve_extension(&package, &data).unwrap()
        else {
            panic!("uninstalled packages are pending");
        };
        assert_eq!(name, "@team/reviewer");
        assert_eq!(
            install,
            "haarness extension install npm:@team/reviewer@1.0.0"
        );
        for id in ["0190-old", "0191-new"] {
            let module = data
                .join("extensions/packages")
                .join(id)
                .join("node_modules/@team/reviewer");
            fs::create_dir_all(&module).unwrap();
            fs::write(
                module.join("package.json"),
                format!("{{\"name\":\"@team/reviewer\",\"main\":\"{id}.js\"}}"),
            )
            .unwrap();
            fs::write(
                module.join(format!("{id}.js")),
                "export default {apiVersion:1,name:'reviewer'};",
            )
            .unwrap();
        }
        let Resolution::Entry(entry) = resolve_extension(&package, &data).unwrap() else {
            panic!("installed packages resolve");
        };
        assert!(
            entry
                .source
                .ends_with("0191-new/node_modules/@team/reviewer/0191-new.js"),
            "{}",
            entry.source.display()
        );
        assert_eq!(entry.options, json!({"strict":true}));
        let bad = ExtensionSpec {
            spec: "npm:../escape".into(),
            options: Value::Null,
            file,
        };
        assert!(resolve_extension(&bad, &data).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn plugin_specs_report_pending_installs_and_config_disables() {
        let root = scratch("config-plugins");
        let paths = Paths::new(Some(&root)).unwrap();
        let workspace = root.join("project");
        fs::create_dir_all(&workspace).unwrap();
        let bundle = root.join("bundle");
        fs::create_dir_all(bundle.join(".claude-plugin")).unwrap();
        fs::write(
            bundle.join(".claude-plugin/plugin.json"),
            r#"{"name":"linter","version":"1.0.0"}"#,
        )
        .unwrap();
        plugins::install(&paths.data, &bundle.display().to_string()).unwrap();
        fs::write(
            workspace.join("haarness.json"),
            r#"{"plugins":["reviewer@internal", {"spec":"linter@local","enabled":false}], "plugin":["docs@marketplace"], "extensions":["example-notify"]}"#,
        )
        .unwrap();
        let declared = declarations(paths.config_dir(), &workspace, true);
        assert_eq!(declared.plugins.len(), 3);
        let pending: Vec<_> = pending_plugins(&paths.data, &declared.plugins)
            .into_iter()
            .map(|plugin| plugin.spec)
            .collect();
        assert_eq!(pending, ["reviewer@internal", "docs@marketplace"]);
        assert_eq!(
            disabled_plugins(paths.config_dir(), &workspace, true),
            BTreeSet::from(["linter".to_owned()])
        );
        assert!(
            disabled_plugins(paths.config_dir(), &workspace, false).is_empty(),
            "untrusted projects declare nothing"
        );
        assert!(
            plugins::all_enabled_roots(&paths.data, paths.config_dir(), &workspace, true)
                .is_empty()
        );
        assert_eq!(
            plugins::all_enabled_roots(&paths.data, paths.config_dir(), &workspace, false).len(),
            1
        );
        let infos = plugins::plugin_infos(&paths.data, paths.config_dir(), &workspace, true);
        assert_eq!(infos.len(), 1);
        assert!(
            !infos[0].enabled,
            "configuration disables discovery and the listing agrees"
        );
        let notices = startup_notices(&paths, &workspace, true);
        assert!(
            notices
                .iter()
                .any(|notice| notice.contains("plugin reviewer@internal")
                    && notice.contains("haarness plugin install reviewer@internal")),
            "{notices:?}"
        );
        assert!(
            notices
                .iter()
                .any(|notice| notice.contains("extension package example-notify")
                    && notice.contains("haarness extension install npm:example-notify")),
            "{notices:?}"
        );
        let layers = jsonconfig::load_layers(paths.config_dir(), None, &workspace, true, &[]);
        assert!(layers.errors.is_empty(), "{:?}", layers.errors);
        fs::write(workspace.join("haarness.json"), r#"{"extensions":[42]}"#).unwrap();
        let layers = jsonconfig::load_layers(paths.config_dir(), None, &workspace, true, &[]);
        assert!(
            layers
                .notices
                .iter()
                .any(|notice| notice.contains("extensions[0]")),
            "{:?}",
            layers.notices
        );
        fs::remove_dir_all(root).unwrap();
    }
}
