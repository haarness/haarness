//! Static, non-executing discovery for workflow files.

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use directories::ProjectDirs;
use haarness_protocol::{
    WorkflowDefinition, WorkflowDialect, WorkflowFinding, WorkflowInvalidDefinition,
    WorkflowListResponse, WorkflowMeta, WorkflowSourceResponse,
};
use serde_json::{Map, Number, Value};
use thiserror::Error;
use tree_sitter::{Node, Parser};

const NON_LITERAL_ERROR: &str = "workflow meta must be statically analyzable (literal values only)";
const FLAGGED_BUILTINS: &[&str] = &[
    "fs",
    "child_process",
    "net",
    "http",
    "https",
    "dns",
    "os",
    "worker_threads",
    "vm",
];
const FLAGGED_BUN_MEMBERS: &[&str] = &["spawn", "spawnSync", "write", "file", "$"];
const PORTABLE_BLOCKED_GLOBALS: &[&str] = &[
    "Bun",
    "Deno",
    "Function",
    "Intl",
    "Atomics",
    "Buffer",
    "SharedArrayBuffer",
    "WebSocket",
    "XMLHttpRequest",
    "clearInterval",
    "clearTimeout",
    "console",
    "crypto",
    "eval",
    "fetch",
    "globalThis",
    "module",
    "navigator",
    "performance",
    "process",
    "queueMicrotask",
    "require",
    "setInterval",
    "setTimeout",
    "Date",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowRootScope {
    Project,
    Global,
    Plugin,
}

/// One workflow directory. Roots are searched in the supplied order; the
/// first definition for an invocation wins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRoot {
    pub path: PathBuf,
    pub scope: WorkflowRootScope,
    pub plugin_name: Option<String>,
    pub legacy: bool,
}

impl WorkflowRoot {
    #[must_use]
    pub fn project(path: impl Into<PathBuf>, legacy: bool) -> Self {
        Self {
            path: path.into(),
            scope: WorkflowRootScope::Project,
            plugin_name: None,
            legacy,
        }
    }

    #[must_use]
    pub fn global(path: impl Into<PathBuf>, legacy: bool) -> Self {
        Self {
            path: path.into(),
            scope: WorkflowRootScope::Global,
            plugin_name: None,
            legacy,
        }
    }

    #[must_use]
    pub fn plugin(path: impl Into<PathBuf>, name: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            scope: WorkflowRootScope::Plugin,
            plugin_name: Some(name.into()),
            legacy: false,
        }
    }
}

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("workflow not found: {0}")]
    NotFound(String),
    #[error("cannot read workflow source {path}: {message}")]
    Read { path: String, message: String },
}

#[derive(Debug, Clone)]
pub struct WorkflowCatalog {
    roots: Vec<WorkflowRoot>,
    invalid: Vec<WorkflowInvalidDefinition>,
    project_config_disabled: bool,
}

impl WorkflowCatalog {
    /// Opt out of project definitions while retaining global and installed plugin roots.
    #[must_use]
    pub fn with_project_config(mut self, enabled: bool) -> Self {
        // Keep scope metadata for permission checks when resuming stored runs.
        self.project_config_disabled = !enabled;
        self
    }

    /// Build a catalog with explicit, testable precedence.
    #[must_use]
    pub fn new(roots: Vec<WorkflowRoot>) -> Self {
        Self {
            roots,
            invalid: Vec::new(),
            project_config_disabled: false,
        }
    }

    /// Add one enabled plugin's workflow directory. Plugin workflow
    /// invocations are namespaced (`plugin:workflow`), so they cannot shadow
    /// project or global definitions with the same display name.
    #[must_use]
    pub fn with_plugin_root(mut self, path: impl Into<PathBuf>, name: impl Into<String>) -> Self {
        let root = WorkflowRoot::plugin(path, name);
        if !self
            .roots
            .iter()
            .any(|candidate| candidate.path == root.path)
        {
            self.roots.push(root);
        }
        self
    }

    /// Mount a Claude-format plugin. The optional `workflows`
    /// manifest field replaces the default `./workflows/` directory and may
    /// name one relative file or a list of relative files/directories.
    #[must_use]
    pub fn with_plugin_mount(
        mut self,
        root: impl Into<PathBuf>,
        fallback_name: impl Into<String>,
    ) -> Self {
        let root = root.into();
        let fallback_name = fallback_name.into();
        let manifest_path = root.join(".claude-plugin/plugin.json");
        let manifest = if manifest_path.is_file() {
            match fs::read_to_string(&manifest_path)
                .map_err(|error| error.to_string())
                .and_then(|source| {
                    serde_json::from_str::<Value>(&source).map_err(|error| error.to_string())
                }) {
                Ok(value) => Some(value),
                Err(error) => {
                    self.invalid.push(plugin_invalid(
                        &manifest_path,
                        &fallback_name,
                        format!("Invalid plugin manifest: {error}"),
                    ));
                    return self;
                }
            }
        } else {
            None
        };
        let plugin_name = manifest
            .as_ref()
            .and_then(|value| value.get("name"))
            .and_then(Value::as_str)
            .unwrap_or(&fallback_name)
            .to_owned();
        let workflow_value = manifest.as_ref().and_then(|value| value.get("workflows"));
        let custom = workflow_value.is_some();
        let entries = match workflow_value {
            None => vec!["./workflows/".to_owned()],
            Some(Value::String(value)) => vec![value.clone()],
            Some(Value::Array(values)) if values.iter().all(Value::is_string) => values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            Some(_) => {
                self.invalid.push(plugin_invalid(
                    &manifest_path,
                    &plugin_name,
                    "Plugin manifest workflows must be a string or array of strings",
                ));
                return self;
            }
        };
        let canonical_root = match root.canonicalize() {
            Ok(path) => path,
            Err(error) => {
                self.invalid.push(plugin_invalid(
                    &root,
                    &plugin_name,
                    format!("Plugin root does not exist: {error}"),
                ));
                return self;
            }
        };
        for entry in entries {
            if !entry.starts_with("./") {
                self.invalid.push(plugin_invalid(
                    &manifest_path,
                    &plugin_name,
                    format!("Plugin workflow path must start with ./: {entry}"),
                ));
                continue;
            }
            let target = root.join(entry.trim_start_matches("./"));
            let canonical = match target.canonicalize() {
                Ok(path) => path,
                Err(error) if custom => {
                    self.invalid.push(plugin_invalid(
                        &target,
                        &plugin_name,
                        format!("Plugin workflow path does not exist: {error}"),
                    ));
                    continue;
                }
                Err(_) => continue,
            };
            if !canonical.starts_with(&canonical_root) {
                self.invalid.push(plugin_invalid(
                    &target,
                    &plugin_name,
                    "Plugin workflow path resolves outside the plugin root",
                ));
                continue;
            }
            if canonical.is_file()
                && !matches!(
                    canonical.extension().and_then(|value| value.to_str()),
                    Some("ts" | "js" | "mjs")
                )
            {
                self.invalid.push(plugin_invalid(
                    &canonical,
                    &plugin_name,
                    "Plugin workflow files must use .js, .mjs, or .ts",
                ));
                continue;
            }
            self = self.with_plugin_root(canonical, plugin_name.clone());
        }
        self
    }

    /// Compatibility roots ordered from most to least specific. Project
    /// definitions always win over global definitions, independent of their
    /// absolute path's lexical ordering.
    #[must_use]
    pub fn for_workspace(workspace: &Path) -> Self {
        let mut roots = Vec::new();
        // Match Haarness project-config discovery when Haarness starts in a
        // repository subdirectory: the nearest directory wins, then each
        // parent through the current worktree root contributes compatible
        // hidden roots. We deliberately stop at the first `.git` marker so a
        // checkout never inherits an unrelated ancestor repository's config.
        for directory in workflow_project_directories(workspace) {
            roots.extend([
                WorkflowRoot::project(directory.join(".haarness/workflows"), false),
                WorkflowRoot::project(directory.join(".upstream/workflows"), false),
                WorkflowRoot::project(directory.join(".opencode/workflows"), true),
            ]);
        }

        if let Some(root) = std::env::var_os("HAARNESS_DATA_DIR") {
            roots.push(WorkflowRoot::global(
                PathBuf::from(root).join("config/workflows"),
                false,
            ));
        } else if let Some(project) = ProjectDirs::from("dev", "haarness", "haarness") {
            roots.push(WorkflowRoot::global(
                project.config_dir().join("workflows"),
                false,
            ));
        }

        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            roots.extend([
                WorkflowRoot::global(home.join(".upstream/workflows"), false),
                WorkflowRoot::global(home.join(".opencode/workflows"), true),
                WorkflowRoot::global(home.join(".config/upstream/workflows"), false),
                WorkflowRoot::global(home.join(".config/opencode/workflows"), true),
            ]);
        }
        if let Some(root) = std::env::var_os("UPSTREAM_CONFIG_DIR") {
            roots.push(WorkflowRoot::global(
                PathBuf::from(root).join("workflows"),
                false,
            ));
        }

        let mut seen = HashSet::new();
        roots.retain(|root| seen.insert(root.path.clone()));
        Self::new(roots)
    }

    #[must_use]
    pub fn list(&self) -> WorkflowListResponse {
        let mut definitions = HashMap::<String, WorkflowDefinition>::new();
        let mut invalid = self.invalid.clone();

        for root in &self.roots {
            if self.project_config_disabled && root.scope == WorkflowRootScope::Project {
                continue;
            }
            for candidate in candidates(root, &mut invalid) {
                match parse_definition(&candidate, root) {
                    Ok(definition) => {
                        definitions
                            .entry(definition.invocation.clone())
                            .or_insert(definition);
                    }
                    Err(error) => invalid.push(WorkflowInvalidDefinition {
                        path: candidate.display().to_string(),
                        plugin_name: root.plugin_name.clone(),
                        legacy: root.legacy,
                        error,
                    }),
                }
            }
        }

        let mut workflows = definitions.into_values().collect::<Vec<_>>();
        workflows.sort_by(|left, right| left.invocation.cmp(&right.invocation));
        invalid.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.error.cmp(&right.error))
        });
        WorkflowListResponse { workflows, invalid }
    }

    pub fn read_source(&self, invocation: &str) -> Result<WorkflowSourceResponse, CatalogError> {
        let definition = self
            .list()
            .workflows
            .into_iter()
            .find(|definition| {
                definition.invocation == invocation
                    || (definition.plugin_name.is_none() && definition.name == invocation)
            })
            .ok_or_else(|| CatalogError::NotFound(invocation.to_owned()))?;
        let source = fs::read_to_string(&definition.path).map_err(|error| CatalogError::Read {
            path: definition.path.clone(),
            message: error.to_string(),
        })?;
        Ok(WorkflowSourceResponse { definition, source })
    }

    /// Whether an installed source came from a project-scoped discovery root.
    /// Callers use this instead of comparing only with the process workspace:
    /// a trusted project root may be an ancestor when Haarness starts inside
    /// a repository subdirectory.
    #[must_use]
    pub fn is_project_path(&self, path: &Path) -> bool {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.roots.iter().any(|root| {
            if root.scope != WorkflowRootScope::Project {
                return false;
            }
            let candidate = root
                .path
                .canonicalize()
                .unwrap_or_else(|_| root.path.clone());
            canonical == candidate || canonical.starts_with(candidate)
        })
    }

    /// Inspect inline workflow source with the same static parser used during
    /// discovery. This never imports or evaluates the module. The returned
    /// definition has no installed path and is suitable for a durable source
    /// snapshot owned by the workflow coordinator.
    pub fn inspect_source(
        &self,
        source: &str,
        label: impl Into<String>,
    ) -> Result<WorkflowDefinition, CatalogError> {
        let label = label.into();
        let (meta, dialect, findings) =
            parse_source(source).map_err(|message| CatalogError::Read {
                path: label.clone(),
                message,
            })?;
        Ok(WorkflowDefinition {
            name: meta.name.clone(),
            invocation: invocation(&meta.name, None),
            plugin_name: None,
            dialect,
            path: label,
            legacy: false,
            meta,
            findings,
        })
    }

    /// Resolve an explicit path only when it is already one of the catalog's
    /// canonical discoveries. This preserves the path/start compatibility
    /// surface without turning workflow start into an arbitrary file reader.
    pub fn read_catalog_path(&self, path: &Path) -> Result<WorkflowSourceResponse, CatalogError> {
        let canonical = path.canonicalize().map_err(|error| CatalogError::Read {
            path: path.display().to_string(),
            message: error.to_string(),
        })?;
        let definition = self
            .list()
            .workflows
            .into_iter()
            .find(|definition| Path::new(&definition.path) == canonical)
            .ok_or_else(|| CatalogError::NotFound(path.display().to_string()))?;
        let source = fs::read_to_string(&canonical).map_err(|error| CatalogError::Read {
            path: canonical.display().to_string(),
            message: error.to_string(),
        })?;
        Ok(WorkflowSourceResponse { definition, source })
    }
}

fn candidates(root: &WorkflowRoot, invalid: &mut Vec<WorkflowInvalidDefinition>) -> Vec<PathBuf> {
    let canonical_root = match root.path.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            invalid.push(WorkflowInvalidDefinition {
                path: root.path.display().to_string(),
                plugin_name: root.plugin_name.clone(),
                legacy: root.legacy,
                error: error.to_string(),
            });
            return Vec::new();
        }
    };
    if canonical_root.is_file() {
        return vec![canonical_root];
    }
    let Ok(entries) = fs::read_dir(&canonical_root) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("ts" | "js" | "mjs")
        ) {
            continue;
        }
        let canonical = match path.canonicalize() {
            Ok(path) if path.starts_with(&canonical_root) => path,
            Ok(_) => {
                invalid.push(WorkflowInvalidDefinition {
                    path: path.display().to_string(),
                    plugin_name: root.plugin_name.clone(),
                    legacy: root.legacy,
                    error: "workflow symlink escapes its discovery root".into(),
                });
                continue;
            }
            Err(error) => {
                invalid.push(WorkflowInvalidDefinition {
                    path: path.display().to_string(),
                    plugin_name: root.plugin_name.clone(),
                    legacy: root.legacy,
                    error: error.to_string(),
                });
                continue;
            }
        };
        if canonical.is_file() {
            files.push(canonical);
        }
    }
    files.sort();
    files
}

fn plugin_invalid(
    path: &Path,
    plugin_name: &str,
    error: impl Into<String>,
) -> WorkflowInvalidDefinition {
    WorkflowInvalidDefinition {
        path: path.display().to_string(),
        plugin_name: Some(plugin_name.to_owned()),
        legacy: false,
        error: error.into(),
    }
}

fn parse_definition(path: &Path, root: &WorkflowRoot) -> Result<WorkflowDefinition, String> {
    let source = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let (meta, dialect, findings) = parse_source(&source)?;
    let invocation = invocation(&meta.name, root.plugin_name.as_deref());
    Ok(WorkflowDefinition {
        name: meta.name.clone(),
        invocation,
        plugin_name: root.plugin_name.clone(),
        dialect,
        path: path.display().to_string(),
        legacy: root.legacy,
        meta,
        findings,
    })
}

fn invocation(name: &str, plugin: Option<&str>) -> String {
    let name = slug(name);
    plugin.map_or_else(|| name.clone(), |plugin| format!("{}:{name}", slug(plugin)))
}

fn slug(value: &str) -> String {
    let trimmed = value.trim();
    let mut result = String::new();
    let mut dash = false;
    for character in trimmed.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
            if dash && !result.is_empty() && !result.ends_with('-') {
                result.push('-');
            }
            dash = false;
            result.push(character);
        } else {
            dash = true;
        }
    }
    while result.ends_with('-') {
        result.pop();
    }
    if result.is_empty() {
        trimmed.to_owned()
    } else {
        result
    }
}

fn parse_source(
    source: &str,
) -> Result<(WorkflowMeta, WorkflowDialect, Vec<WorkflowFinding>), String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
        .map_err(|error| error.to_string())?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| "workflow source could not be parsed".to_owned())?;
    let root = tree.root_node();
    if root.has_error() {
        return Err("workflow source contains a syntax error".into());
    }

    let default_exports = top_level(root)
        .filter(|node| is_default_export(*node, source))
        .count();
    if default_exports > 1 {
        return Err("Multiple default exports; cannot determine the workflow".into());
    }
    let meta_node = find_meta_node(root, source)
        .ok_or_else(|| "Missing meta export (export const meta / export default)".to_owned())?;
    let mut value = extract_value(meta_node, source).map_err(|()| NON_LITERAL_ERROR.to_owned())?;
    normalize_phases(&mut value)?;
    let meta: WorkflowMeta =
        serde_json::from_value(value).map_err(|error| format!("invalid workflow meta: {error}"))?;
    if meta.name.trim().is_empty() {
        return Err("invalid workflow meta: name must not be empty".into());
    }
    let dialect = if is_portable(root, source) {
        WorkflowDialect::ClaudePortable
    } else {
        WorkflowDialect::Native
    };
    let findings = if dialect == WorkflowDialect::Native {
        lint_native_source(root, source)
    } else {
        validate_portable_source(root, source)?;
        Vec::new()
    };
    Ok((meta, dialect, findings))
}

fn workflow_project_directories(workspace: &Path) -> Vec<PathBuf> {
    let mut directories = Vec::new();
    let mut current = Some(workspace);
    while let Some(directory) = current {
        directories.push(directory.to_path_buf());
        if directory.join(".git").exists() {
            return directories;
        }
        current = directory.parent();
    }
    // A workspace outside version control has no authoritative ancestor
    // boundary. Keep it local instead of accidentally treating a home-level
    // native or compatibility directory as trusted project configuration.
    vec![workspace.to_path_buf()]
}

fn validate_portable_source(root: Node<'_>, source: &str) -> Result<(), String> {
    let first = root
        .named_child(0)
        .ok_or_else(|| "Portable workflow source is empty".to_owned())?;
    for statement in top_level(root).skip(1) {
        if statement.kind() == "export_statement" {
            return Err(portable_issue(
                statement,
                "Portable workflows cannot export from the script body",
            ));
        }
    }

    let mut stack = Vec::new();
    let mut cursor = root.walk();
    stack.extend(
        root.named_children(&mut cursor)
            .filter(|node| *node != first),
    );
    while let Some(node) = stack.pop() {
        match node.kind() {
            "import_statement" | "import_require_clause" => {
                return Err(portable_issue(
                    node,
                    "Portable workflows cannot import modules",
                ));
            }
            "call_expression"
                if node
                    .child_by_field_name("function")
                    .is_some_and(|function| {
                        matches!(function.kind(), "import" | "import_keyword")
                            || (function.kind() == "identifier"
                                && text(function, source) == "import")
                    }) =>
            {
                return Err(portable_issue(
                    node,
                    "Portable workflows cannot import modules",
                ));
            }
            "identifier" | "shorthand_property_identifier" => {
                let name = text(node, source);
                if portable_identifier_is_reference(node)
                    && PORTABLE_BLOCKED_GLOBALS.contains(&name)
                {
                    return Err(portable_issue(
                        node,
                        &format!("Portable workflow global {name} is unavailable"),
                    ));
                }
            }
            "member_expression" => {
                let object = node.child_by_field_name("object");
                let property = node.child_by_field_name("property");
                if object.is_some_and(|value| {
                    value.kind() == "identifier" && text(value, source) == "Math"
                }) && property.is_some_and(|value| text(value, source) == "random")
                {
                    return Err(portable_issue(
                        node,
                        "Portable workflows cannot access Math.random",
                    ));
                }
            }
            "subscript_expression" => {
                let object = node.child_by_field_name("object");
                let index = node.child_by_field_name("index");
                if object.is_some_and(|value| {
                    value.kind() == "identifier" && text(value, source) == "Math"
                }) && index
                    .and_then(|value| decode_string(text(value, source)).ok())
                    .as_deref()
                    == Some("random")
                {
                    return Err(portable_issue(
                        node,
                        "Portable workflows cannot access Math.random",
                    ));
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    Ok(())
}

fn portable_identifier_is_reference(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return true;
    };
    if matches!(parent.kind(), "member_expression" | "subscript_expression")
        && parent
            .child_by_field_name("property")
            .is_some_and(|property| property.id() == node.id())
    {
        return false;
    }
    if parent.kind() == "pair"
        && parent
            .child_by_field_name("key")
            .is_some_and(|key| key.id() == node.id())
    {
        return false;
    }
    !(parent.kind() == "method_definition"
        && parent
            .child_by_field_name("name")
            .is_some_and(|name| name.id() == node.id()))
}

fn portable_issue(node: Node<'_>, message: &str) -> String {
    let position = node.start_position();
    format!(
        "{message} at line {}, column {}",
        position.row + 1,
        position.column + 1
    )
}

fn lint_native_source(root: Node<'_>, source: &str) -> Vec<WorkflowFinding> {
    let mut findings = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "import_statement" => {
                if let Some(require) = named_child_of_kind(node, "import_require_clause") {
                    if require
                        .child_by_field_name("source")
                        .and_then(|value| string_literal(value, source))
                        .is_some_and(flagged_module)
                    {
                        add_finding(&mut findings, node, "node-builtin-require", source);
                    }
                } else if node
                    .child_by_field_name("source")
                    .and_then(|value| string_literal(value, source))
                    .is_some_and(flagged_module)
                {
                    add_finding(&mut findings, node, "node-builtin-import", source);
                }
            }
            "export_statement" => {
                if node
                    .child_by_field_name("source")
                    .and_then(|value| string_literal(value, source))
                    .is_some_and(flagged_module)
                {
                    add_finding(&mut findings, node, "node-builtin-import", source);
                }
            }
            "call_expression" => lint_call(node, source, &mut findings),
            "member_expression" => {
                let object = node.child_by_field_name("object");
                let property = node.child_by_field_name("property");
                if object.is_some_and(|value| {
                    value.kind() == "identifier" && text(value, source) == "Bun"
                }) && property
                    .is_some_and(|value| FLAGGED_BUN_MEMBERS.contains(&text(value, source)))
                {
                    add_finding(&mut findings, node, "bun-api", source);
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        let children = node.named_children(&mut cursor).collect::<Vec<_>>();
        stack.extend(children.into_iter().rev());
    }
    findings
}

fn lint_call(node: Node<'_>, source: &str, findings: &mut Vec<WorkflowFinding>) {
    let Some(function) = node.child_by_field_name("function") else {
        return;
    };
    let argument = node
        .child_by_field_name("arguments")
        .and_then(|arguments| arguments.named_child(0));
    if function.kind() == "import" {
        match argument.and_then(|value| string_literal(value, source)) {
            Some(specifier) if flagged_module(specifier) => {
                add_finding(findings, node, "node-builtin-import", source);
            }
            Some(_) => {}
            None => add_finding(findings, node, "dynamic-import", source),
        }
    } else if function.kind() == "identifier" && text(function, source) == "require" {
        match argument.and_then(|value| string_literal(value, source)) {
            Some(specifier) if flagged_module(specifier) => {
                add_finding(findings, node, "node-builtin-require", source);
            }
            Some(_) => {}
            None => add_finding(findings, node, "node-builtin-require", source),
        }
    } else if function.kind() == "identifier" && text(function, source) == "fetch" {
        add_finding(findings, node, "fetch", source);
    }
}

fn named_child_of_kind<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| child.kind() == kind)
}

fn string_literal<'source>(node: Node<'_>, source: &'source str) -> Option<&'source str> {
    if node.kind() != "string" {
        return None;
    }
    let value = text(node, source);
    value.get(1..value.len().checked_sub(1)?)
}

fn flagged_module(specifier: &str) -> bool {
    let specifier = specifier.strip_prefix("node:").unwrap_or(specifier);
    let root = specifier.split('/').next().unwrap_or(specifier);
    FLAGGED_BUILTINS.contains(&root)
}

fn add_finding(findings: &mut Vec<WorkflowFinding>, node: Node<'_>, rule: &str, source: &str) {
    let compact = text(node, source)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let text = if compact.chars().count() > 120 {
        let mut value = compact.chars().take(117).collect::<String>();
        value.push_str("...");
        value
    } else {
        compact
    };
    findings.push(WorkflowFinding {
        line: node.start_position().row as u64 + 1,
        rule: rule.to_owned(),
        text,
    });
}

fn top_level(root: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    (0..root.named_child_count()).filter_map(move |index| root.named_child(index))
}

fn is_default_export(node: Node<'_>, source: &str) -> bool {
    node.kind() == "export_statement"
        && text(node, source)
            .trim_start()
            .strip_prefix("export")
            .is_some_and(|value| value.trim_start().starts_with("default"))
}

fn find_meta_node<'tree>(root: Node<'tree>, source: &str) -> Option<Node<'tree>> {
    for statement in top_level(root) {
        if statement.kind() != "export_statement" {
            continue;
        }
        if is_default_export(statement, source) {
            let expression = statement.named_child(0).map(unwrap_expression)?;
            if expression.kind() == "object" {
                return object_property(expression, "meta", source).or(Some(expression));
            }
            if expression.kind() == "call_expression" {
                let arguments = expression.child_by_field_name("arguments")?;
                return arguments
                    .named_child(0)
                    .map(unwrap_expression)
                    .filter(|node| node.kind() == "object");
            }
            continue;
        }
        let mut cursor = statement.walk();
        for child in statement.named_children(&mut cursor) {
            if let Some(value) = exported_meta_declaration(child, source) {
                return Some(value);
            }
        }
    }
    None
}

fn exported_meta_declaration<'tree>(node: Node<'tree>, source: &str) -> Option<Node<'tree>> {
    if node.kind() == "variable_declarator"
        && node
            .child_by_field_name("name")
            .is_some_and(|name| text(name, source) == "meta")
    {
        return node
            .child_by_field_name("value")
            .map(unwrap_expression)
            .filter(|value| value.kind() == "object");
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(value) = exported_meta_declaration(child, source) {
            return Some(value);
        }
    }
    None
}

fn is_portable(root: Node<'_>, source: &str) -> bool {
    let Some(first) = root.named_child(0) else {
        return false;
    };
    if first.kind() != "export_statement" || exported_meta_declaration(first, source).is_none() {
        return false;
    }
    !top_level(root).any(|statement| exported_run(statement, source))
}

fn exported_run(statement: Node<'_>, source: &str) -> bool {
    if statement.kind() != "export_statement" {
        return false;
    }
    if is_default_export(statement, source) {
        return true;
    }
    let mut stack = vec![statement];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "function_declaration" | "variable_declarator")
            && node
                .child_by_field_name("name")
                .is_some_and(|name| text(name, source) == "run")
        {
            return true;
        }
        if node.kind() == "export_clause" && token_present(text(node, source), "run") {
            return true;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    false
}

fn token_present(value: &str, token: &str) -> bool {
    value
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .any(|part| part == token)
}

fn unwrap_expression(mut node: Node<'_>) -> Node<'_> {
    while matches!(
        node.kind(),
        "parenthesized_expression" | "as_expression" | "satisfies_expression" | "type_assertion"
    ) {
        let Some(next) = node
            .child_by_field_name("expression")
            .or_else(|| node.named_child(0))
        else {
            break;
        };
        node = next;
    }
    node
}

fn object_property<'tree>(object: Node<'tree>, wanted: &str, source: &str) -> Option<Node<'tree>> {
    let mut cursor = object.walk();
    for property in object.named_children(&mut cursor) {
        if property.kind() != "pair" {
            continue;
        }
        let key = property.child_by_field_name("key")?;
        if property_key(key, source).as_deref() == Some(wanted) {
            return property.child_by_field_name("value").map(unwrap_expression);
        }
    }
    None
}

fn extract_value(node: Node<'_>, source: &str) -> Result<Value, ()> {
    let node = unwrap_expression(node);
    match node.kind() {
        "string" | "template_string" => decode_string(text(node, source)).map(Value::String),
        "number" => parse_number(text(node, source)).map(Value::Number),
        "true" => Ok(Value::Bool(true)),
        "false" => Ok(Value::Bool(false)),
        "null" => Ok(Value::Null),
        "unary_expression" => {
            let raw = text(node, source).trim();
            let (negative, number) = raw
                .strip_prefix('-')
                .map(|value| (true, value))
                .or_else(|| raw.strip_prefix('+').map(|value| (false, value)))
                .ok_or(())?;
            let mut value = parse_f64(number)?;
            if negative {
                value = -value;
            }
            Number::from_f64(value).map(Value::Number).ok_or(())
        }
        "array" => {
            let mut values = Vec::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                values.push(extract_value(child, source)?);
            }
            Ok(Value::Array(values))
        }
        "object" => {
            let mut values = Map::new();
            let mut cursor = node.walk();
            for property in node.named_children(&mut cursor) {
                if property.kind() == "method_definition" {
                    continue;
                }
                if property.kind() != "pair" {
                    return Err(());
                }
                let key = property
                    .child_by_field_name("key")
                    .and_then(|key| property_key(key, source))
                    .ok_or(())?;
                let value = property.child_by_field_name("value").ok_or(())?;
                let unwrapped = unwrap_expression(value);
                if key == "run"
                    && matches!(unwrapped.kind(), "arrow_function" | "function_expression")
                {
                    continue;
                }
                values.insert(key, extract_value(value, source)?);
            }
            Ok(Value::Object(values))
        }
        _ => Err(()),
    }
}

fn property_key(node: Node<'_>, source: &str) -> Option<String> {
    match node.kind() {
        "property_identifier" | "identifier" | "number" => Some(text(node, source).to_owned()),
        "string" => decode_string(text(node, source)).ok(),
        _ => None,
    }
}

fn parse_number(raw: &str) -> Result<Number, ()> {
    let normalized = raw.replace('_', "");
    if !normalized.contains(['.', 'e', 'E']) {
        if let Ok(value) = normalized.parse::<i64>() {
            return Ok(Number::from(value));
        }
        if let Ok(value) = normalized.parse::<u64>() {
            return Ok(Number::from(value));
        }
    }
    Number::from_f64(parse_f64(&normalized)?).ok_or(())
}

fn parse_f64(raw: &str) -> Result<f64, ()> {
    raw.replace('_', "").parse::<f64>().map_err(|_| ())
}

fn decode_string(raw: &str) -> Result<String, ()> {
    let bytes = raw.as_bytes();
    if bytes.len() < 2 {
        return Err(());
    }
    let quote = bytes[0];
    if !matches!(quote, b'\'' | b'"' | b'`') || bytes[bytes.len() - 1] != quote {
        return Err(());
    }
    if quote == b'`' && raw.contains("${") {
        return Err(());
    }
    if quote == b'"' {
        return serde_json::from_str(raw).map_err(|_| ());
    }
    let mut result = String::new();
    let mut chars = raw[1..raw.len() - 1].chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            result.push(character);
            continue;
        }
        let escaped = chars.next().ok_or(())?;
        match escaped {
            'n' => result.push('\n'),
            'r' => result.push('\r'),
            't' => result.push('\t'),
            'b' => result.push('\u{0008}'),
            'f' => result.push('\u{000c}'),
            'v' => result.push('\u{000b}'),
            '0' => result.push('\0'),
            '\\' => result.push('\\'),
            '\'' => result.push('\''),
            '"' => result.push('"'),
            '`' => result.push('`'),
            other => result.push(other),
        }
    }
    Ok(result)
}

fn normalize_phases(value: &mut Value) -> Result<(), String> {
    let Some(phases) = value
        .as_object_mut()
        .and_then(|object| object.get_mut("phases"))
    else {
        return Ok(());
    };
    let array = phases
        .as_array_mut()
        .ok_or_else(|| "invalid workflow meta: phases must be an array".to_owned())?;
    for phase in array {
        if let Value::String(title) = phase {
            *phase = serde_json::json!({"title": title});
        }
    }
    Ok(())
}

fn text<'source>(node: Node<'_>, source: &'source str) -> &'source str {
    &source[node.byte_range()]
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{WorkflowCatalog, WorkflowDialect, WorkflowRoot, workflow_project_directories};

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("haarness-workflow-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn project_definition_wins_over_lexically_earlier_global_path() {
        let temp = TempDir::new("precedence");
        let global = temp.0.join("aaa-global");
        let project = temp.0.join("zzz-project");
        fs::create_dir_all(&global).unwrap();
        fs::create_dir_all(&project).unwrap();
        fs::write(
            global.join("release.ts"),
            "export default workflow({ name: 'release', description: 'global', run() {} })",
        )
        .unwrap();
        fs::write(
            project.join("release.ts"),
            "export default workflow({ name: 'release', description: 'project', run() {} })",
        )
        .unwrap();

        let result = WorkflowCatalog::new(vec![
            WorkflowRoot::project(&project, false),
            WorkflowRoot::global(&global, false),
        ])
        .list();
        assert_eq!(result.workflows.len(), 1);
        assert_eq!(
            result.workflows[0].meta.description.as_deref(),
            Some("project")
        );
        assert!(result.workflows[0].path.contains("zzz-project"));
    }

    #[test]
    fn disabled_project_config_hides_definitions_but_retains_scope_for_stored_runs() {
        let temp = TempDir::new("disabled-project-config");
        let global = temp.0.join("global");
        let project = temp.0.join("project");
        fs::create_dir_all(&global).unwrap();
        fs::create_dir_all(&project).unwrap();
        fs::write(
            global.join("global.ts"),
            "export default workflow({ name: 'global', run() {} })",
        )
        .unwrap();
        let project_source = project.join("project.ts");
        fs::write(
            &project_source,
            "export default workflow({ name: 'project', run() {} })",
        )
        .unwrap();

        let catalog = WorkflowCatalog::new(vec![
            WorkflowRoot::project(&project, false),
            WorkflowRoot::global(&global, false),
        ])
        .with_project_config(false);
        let result = catalog.list();
        assert_eq!(result.workflows.len(), 1, "{result:?}");
        assert_eq!(result.workflows[0].name, "global");
        assert!(catalog.read_source("project").is_err());
        assert!(catalog.is_project_path(&project_source));
    }

    #[test]
    fn absent_optional_roots_are_not_invalid_workflow_definitions() {
        let temp = TempDir::new("missing-root");
        let result = WorkflowCatalog::new(vec![WorkflowRoot::project(
            temp.0.join("does-not-exist"),
            false,
        )])
        .list();

        assert!(result.workflows.is_empty());
        assert!(result.invalid.is_empty(), "{result:?}");
    }

    #[test]
    fn parses_both_native_metadata_forms_without_executing_source() {
        let temp = TempDir::new("literal");
        fs::write(
            temp.0.join("native.ts"),
            r#"
                import { workflow } from "@haarness-ai/plugin"
                export default workflow({
                  name: "Release Train",
                  whenToUse: `shipping`,
                  phases: ["verify", { title: "ship", detail: "carefully" }],
                  arguments: { dryRun: { type: "boolean", default: true } },
                  async run() { throw new Error("must never execute during discovery") },
                })
            "#,
        )
        .unwrap();
        fs::write(
            temp.0.join("portable.js"),
            "export const meta = { name: 'portable' };\nreturn await agent('hello')",
        )
        .unwrap();

        let result = WorkflowCatalog::new(vec![WorkflowRoot::project(&temp.0, false)]).list();
        assert!(result.invalid.is_empty(), "{:?}", result.invalid);
        assert_eq!(result.workflows.len(), 2);
        let native = result
            .workflows
            .iter()
            .find(|workflow| workflow.name == "Release Train")
            .unwrap();
        assert_eq!(native.invocation, "release-train");
        assert_eq!(native.dialect, WorkflowDialect::Native);
        assert_eq!(native.meta.phases.len(), 2);
        let portable = result
            .workflows
            .iter()
            .find(|workflow| workflow.name == "portable")
            .unwrap();
        assert_eq!(portable.dialect, WorkflowDialect::ClaudePortable);
    }

    #[test]
    fn native_findings_are_ast_based_and_inline_inspection_matches_discovery() {
        let temp = TempDir::new("native-findings");
        let source = r#"
import fs from "node:fs/promises"
export { request } from "https"
import child = require("child_process")
export const meta = { name: "native-findings" }
export async function run() {
  const secret = process.env.SECRET
  const harmless = "fetch('https://example.com') and Bun.spawn()"
  await import(moduleName)
  await import("node:dns/promises")
  require(moduleName)
  require("path")
  await fetch("https://example.com")
  await client.fetch("https://example.com")
  Bun.spawn(["true"])
  Bun.sleep(1)
  return { secret, harmless }
}
"#;
        fs::write(temp.0.join("native.ts"), source).unwrap();
        let catalog = WorkflowCatalog::new(vec![WorkflowRoot::project(&temp.0, false)]);

        let discovered = catalog.list().workflows.pop().unwrap();
        let inline = catalog.inspect_source(source, "<inline-findings>").unwrap();
        assert_eq!(discovered.findings, inline.findings);
        assert_eq!(
            discovered
                .findings
                .iter()
                .map(|finding| (finding.line, finding.rule.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (2, "node-builtin-import"),
                (3, "node-builtin-import"),
                (4, "node-builtin-require"),
                (9, "dynamic-import"),
                (10, "node-builtin-import"),
                (11, "node-builtin-require"),
                (13, "fetch"),
                (15, "bun-api"),
            ]
        );
        assert!(
            discovered
                .findings
                .iter()
                .all(|finding| !finding.text.contains('\n'))
        );
    }

    #[test]
    fn portable_sources_do_not_report_native_capability_findings() {
        let source = r#"
export const meta = { name: "portable-findings" }
// These names are denied by the portable VM, not native-source findings.
const prose = "process.env, fetch(), Bun.spawn(), import(name)"
return prose
"#;
        let definition = WorkflowCatalog::new(Vec::new())
            .inspect_source(source, "<portable-findings>")
            .unwrap();

        assert_eq!(definition.dialect, WorkflowDialect::ClaudePortable);
        assert!(definition.findings.is_empty());
    }

    #[test]
    fn invalid_portable_capabilities_are_rejected_during_discovery() {
        let temp = TempDir::new("portable-validation");
        for (name, body) in [
            ("process", "return process.env.SECRET"),
            ("date", "return Date.now()"),
            ("random", "return Math['random']()"),
            ("import", "return import('./unsafe.js')"),
            ("export", "export const unsafe = true\nreturn unsafe"),
        ] {
            fs::write(
                temp.0.join(format!("{name}.js")),
                format!("export const meta = {{ name: '{name}' }};\n{body}"),
            )
            .unwrap();
        }
        fs::write(
            temp.0.join("safe.js"),
            r#"export const meta = { name: "safe" };
const labels = { process: "not a global", fetch: "not a call" }
const prose = "Date.now(), process.env, Math.random()"
return client.fetch(labels.fetch + prose)
"#,
        )
        .unwrap();

        let result = WorkflowCatalog::new(vec![WorkflowRoot::project(&temp.0, false)]).list();
        assert_eq!(result.workflows.len(), 1, "{result:?}");
        assert_eq!(result.workflows[0].invocation, "safe");
        assert_eq!(result.invalid.len(), 5, "{result:?}");
        assert!(
            result
                .invalid
                .iter()
                .any(|item| item.error.contains("global process"))
        );
        assert!(
            result
                .invalid
                .iter()
                .any(|item| item.error.contains("Math.random"))
        );
        assert!(
            result
                .invalid
                .iter()
                .any(|item| item.error.contains("cannot import"))
        );
    }

    #[test]
    fn workspace_catalog_walks_to_the_nearest_worktree_root() {
        let temp = TempDir::new("ancestor-roots");
        fs::create_dir_all(temp.0.join(".git")).unwrap();
        let root_workflows = temp.0.join(".upstream/workflows");
        let nested = temp.0.join("crates/demo");
        let nested_workflows = nested.join(".haarness/workflows");
        fs::create_dir_all(&root_workflows).unwrap();
        fs::create_dir_all(&nested_workflows).unwrap();
        fs::write(
            root_workflows.join("root.ts"),
            "export default workflow({ name: 'root-only', run() {} })",
        )
        .unwrap();
        fs::write(
            root_workflows.join("shared.ts"),
            "export default workflow({ name: 'shared', description: 'root', run() {} })",
        )
        .unwrap();
        fs::write(
            nested_workflows.join("shared.ts"),
            "export default workflow({ name: 'shared', description: 'nested', run() {} })",
        )
        .unwrap();

        let catalog = WorkflowCatalog::for_workspace(&nested);
        let result = catalog.list();
        assert!(
            result
                .workflows
                .iter()
                .any(|workflow| workflow.invocation == "root-only")
        );
        let shared = result
            .workflows
            .iter()
            .find(|workflow| workflow.invocation == "shared")
            .unwrap();
        assert_eq!(shared.meta.description.as_deref(), Some("nested"));
        let root_only = result
            .workflows
            .iter()
            .find(|workflow| workflow.invocation == "root-only")
            .unwrap();
        assert!(catalog.is_project_path(std::path::Path::new(&root_only.path)));
    }

    #[test]
    fn workspace_without_a_worktree_does_not_inherit_ancestor_project_roots() {
        let temp = TempDir::new("no-ancestor-roots");
        let nested = temp.0.join("standalone/workspace");
        fs::create_dir_all(&nested).unwrap();

        assert_eq!(
            workflow_project_directories(&nested),
            vec![nested],
            "a non-repository workspace must not trust unrelated ancestors"
        );
    }

    #[test]
    fn dynamic_or_malformed_metadata_is_reported_not_executed() {
        let temp = TempDir::new("invalid");
        fs::write(
            temp.0.join("dynamic.ts"),
            "export const meta = { name: process.env.WORKFLOW_NAME }; export async function run() {}",
        )
        .unwrap();
        fs::write(temp.0.join("missing.ts"), "export async function run() {}").unwrap();

        let result = WorkflowCatalog::new(vec![WorkflowRoot::project(&temp.0, false)]).list();
        assert!(result.workflows.is_empty());
        assert_eq!(result.invalid.len(), 2);
        assert!(
            result
                .invalid
                .iter()
                .any(|item| item.error.contains("literal"))
        );
        assert!(
            result
                .invalid
                .iter()
                .any(|item| item.error.contains("Missing meta"))
        );
    }

    #[test]
    fn read_source_only_accepts_a_discovered_invocation() {
        let temp = TempDir::new("source");
        let source = "export default workflow({ name: 'demo', run() {} })";
        fs::write(temp.0.join("demo.ts"), source).unwrap();
        let catalog = WorkflowCatalog::new(vec![WorkflowRoot::project(&temp.0, false)]);

        assert_eq!(catalog.read_source("demo").unwrap().source, source);
        assert!(catalog.read_source("../secret").is_err());
    }

    #[test]
    fn plugin_workflows_are_namespaced_and_do_not_shadow_project_workflows() {
        let temp = TempDir::new("plugin");
        let project = temp.0.join("project");
        let plugin = temp.0.join("plugin");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&plugin).unwrap();
        fs::write(
            project.join("release.ts"),
            "export default workflow({ name: 'release', run() {} })",
        )
        .unwrap();
        fs::write(
            plugin.join("release.ts"),
            "export default workflow({ name: 'release', run() {} })",
        )
        .unwrap();

        let catalog = WorkflowCatalog::new(vec![WorkflowRoot::project(&project, false)])
            .with_plugin_root(&plugin, "shipping tools");
        let result = catalog.list();

        assert_eq!(result.workflows.len(), 2);
        assert!(
            result
                .workflows
                .iter()
                .any(|item| item.invocation == "release")
        );
        assert!(
            result
                .workflows
                .iter()
                .any(|item| item.invocation == "shipping-tools:release")
        );
        assert_eq!(
            catalog
                .read_source("shipping-tools:release")
                .unwrap()
                .definition
                .plugin_name
                .as_deref(),
            Some("shipping tools")
        );
    }

    #[test]
    fn plugin_manifest_supports_custom_files_and_rejects_escape_paths() {
        let temp = TempDir::new("plugin-manifest");
        let plugin = temp.0.join("plugin");
        fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
        fs::create_dir_all(plugin.join("flows")).unwrap();
        fs::write(
            plugin.join(".claude-plugin/plugin.json"),
            r#"{"name":"review-kit","workflows":["./flows/review.ts","./../outside.ts"]}"#,
        )
        .unwrap();
        fs::write(
            plugin.join("flows/review.ts"),
            "export const meta = { name: 'review' }; return args",
        )
        .unwrap();
        fs::write(
            temp.0.join("outside.ts"),
            "export const meta = { name: 'outside' }; return args",
        )
        .unwrap();

        let result = WorkflowCatalog::new(Vec::new())
            .with_plugin_mount(&plugin, "fallback")
            .list();

        assert_eq!(result.workflows.len(), 1);
        assert_eq!(result.workflows[0].invocation, "review-kit:review");
        assert_eq!(result.invalid.len(), 1);
        assert!(result.invalid[0].error.contains("outside the plugin root"));
    }
}
