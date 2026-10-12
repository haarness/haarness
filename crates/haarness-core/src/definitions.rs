//! Shared Claude/Agent Skills metadata and plugin component discovery.
use crate::{CoreError, CoreResult};
use haarness_protocol::DefinitionExecution;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fmt::Write as _,
    path::{Path, PathBuf},
};

pub fn markdown(content: &str) -> CoreResult<(BTreeMap<String, String>, &str)> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let Some(rest) = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))
    else {
        return Ok((BTreeMap::new(), content));
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            let front = &rest[..offset];
            let docs = yaml_rust::YamlLoader::load_from_str(front)
                .or_else(|original| {
                    // Anthropic's legacy public agents put unindented XML examples
                    // in description. Repair only that scalar; malformed policy
                    // fields still fail YAML parsing instead of becoming permissive.
                    yaml_rust::YamlLoader::load_from_str(&legacy_description(front))
                        .or_else(|_| yaml_rust::YamlLoader::load_from_str(&text_values(front)))
                        .map_err(|_| original)
                })
                .map_err(|e| CoreError::Invalid(format!("invalid definition frontmatter: {e}")))?;
            let value = docs.first().map(yaml_json).unwrap_or(Value::Null);
            if !value.is_object() && !value.is_null() {
                return Err(CoreError::Invalid(
                    "definition frontmatter must be a mapping".into(),
                ));
            }
            let front = value
                .as_object()
                .into_iter()
                .flatten()
                .map(|(k, v)| (k.clone(), front_value(v)))
                .collect();
            return Ok((front, &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    Err(CoreError::Invalid(
        "definition frontmatter is missing its closing delimiter".into(),
    ))
}

fn front_value(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

/// Whether `value` can follow `key: ` unquoted and read back unchanged under
/// YAML's own rules. A writer must not lean on the repairs [`markdown`]
/// applies, so the file stays valid for every other reader.
pub fn plain_value(value: &str) -> bool {
    !value.contains(['\n', '\r'])
        && yaml_rust::YamlLoader::load_from_str(&format!("value: {value}"))
            .ok()
            .and_then(|docs| docs.first().map(yaml_json))
            .and_then(|front| front.get("value").map(front_value))
            .is_some_and(|read| read == value)
}
fn legacy_description(front: &str) -> String {
    let lines = front.lines().collect::<Vec<_>>();
    let Some(start) = lines.iter().position(|l| l.starts_with("description:")) else {
        return front.into();
    };
    let fields = [
        "name",
        "model",
        "color",
        "tools",
        "disallowedTools",
        "permission",
        "permissions",
        "permissionMode",
        "mode",
        "maxTurns",
        "skills",
        "mcpServers",
        "hooks",
        "memory",
        "background",
        "isolation",
        "allowed-tools",
        "context",
        "agent",
        "user-invocable",
        "disable-model-invocation",
        "argument-hint",
        "license",
        "compatibility",
        "metadata",
    ];
    let end = (start + 1..lines.len())
        .find(|&i| {
            lines[i]
                .split_once(':')
                .is_some_and(|(key, _)| fields.contains(&key))
        })
        .unwrap_or(lines.len());
    let mut description = lines[start]
        .trim_start_matches("description:")
        .trim()
        .to_owned();
    for line in &lines[start + 1..end] {
        description.push('\n');
        description.push_str(line);
    }
    let mut output = lines[..start].join("\n");
    if !output.is_empty() {
        output.push('\n');
    }
    output.push_str(&format!(
        "description: {}\n",
        serde_json::to_string(&description).unwrap()
    ));
    output.push_str(&lines[end..].join("\n"));
    output
}

/// Claude Code reads `allowed-tools: Bash(git commit: *)` and the documented
/// `argument-hint: [filename] [format]` as text; YAML rejects both (a colon
/// and space inside a plain scalar, a flow list followed by more text). Quote
/// such a top-level value. A value that opens a collection, alias, anchor,
/// tag, quote or block scalar depends on the rest of the header, so it is
/// never coerced: a malformed policy list still fails the whole file.
fn text_values(front: &str) -> String {
    let field =
        regex::Regex::new(r"^([A-Za-z_][A-Za-z0-9_-]*)[ \t]*:[ \t]+(.+)$").expect("constant regex");
    let placeholders =
        regex::Regex::new(r"^\[[^\[\]]+\](?:[ \t]+\[[^\[\]]+\])+$").expect("constant regex");
    let comment = regex::Regex::new(r"(?:^|[ \t])#.*$").expect("constant regex");
    let colon = regex::Regex::new(r":(?:[ \t]|$)").expect("constant regex");
    front
        .lines()
        .map(|line| {
            let Some(capture) = field.captures(line) else {
                return line.to_owned();
            };
            let (key, value) = (&capture[1], capture[2].trim());
            let text = if key == "argument-hint" && placeholders.is_match(value) {
                value.to_owned()
            } else {
                // Only a colon before whitespace is illegal in a plain scalar;
                // times, URLs and comments keep their YAML meaning.
                let scalar = comment.replace(value, "");
                let scalar = scalar.trim_end();
                if value.starts_with(['[', ']', '{', '}', '"', '\'', '|', '>', '&', '*', '!', '#'])
                    || !colon.is_match(scalar)
                    || yaml_rust::YamlLoader::load_from_str(&format!("probe: {scalar}")).is_ok()
                {
                    return line.to_owned();
                }
                scalar.to_owned()
            };
            format!("{key}: {}", serde_json::to_string(&text).unwrap())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn yaml_json(y: &yaml_rust::Yaml) -> Value {
    use yaml_rust::Yaml;
    match y {
        Yaml::String(s) => Value::String(s.clone()),
        Yaml::Integer(i) => (*i).into(),
        Yaml::Real(s) => serde_json::from_str(s).unwrap_or(Value::Null),
        Yaml::Boolean(b) => (*b).into(),
        Yaml::Array(a) => Value::Array(a.iter().map(yaml_json).collect()),
        Yaml::Hash(h) => Value::Object(
            h.iter()
                .filter_map(|(k, v)| k.as_str().map(|k| (k.into(), yaml_json(v))))
                .collect(),
        ),
        _ => Value::Null,
    }
}
pub fn list(value: Option<&String>) -> Vec<String> {
    let Some(value) = value else { return vec![] };
    if let Ok(Value::Array(values)) = serde_json::from_str(value) {
        return values
            .into_iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
    }
    // Commas separate tool entries; spaces inside Bash(...) are arguments.
    let mut depth = 0;
    let mut start = 0;
    let mut result = Vec::new();
    for (i, c) in value.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if (c == ',' || c.is_whitespace()) && depth == 0 {
            if !value[start..i].trim().is_empty() {
                result.push(value[start..i].trim().into());
            }
            start = i + c.len_utf8();
        }
    }
    if !value[start..].trim().is_empty() {
        result.push(value[start..].trim().into());
    }
    result
}
/// Claude Code's `model` families. Each resolves to the newest release a
/// configured provider lists.
pub const MODEL_FAMILIES: [&str; 4] = ["sonnet", "opus", "haiku", "fable"];

/// The Claude release a model id names, without the routing a catalog adds:
/// `claude-sonnet-5-5`, `anthropic/claude-sonnet-5.5` and
/// `us.anthropic.claude-sonnet-5-5-v1:0` all name `claude-sonnet-5-5`.
pub fn claude_release(id: &str) -> Option<String> {
    let id = id.trim().to_ascii_lowercase();
    let start = id.find("claude-")?;
    if start > 0 && !id[..start].ends_with(['/', '.']) {
        return None;
    }
    let mut release = &id[start..];
    // Vertex pins a snapshot with `@date`; Bedrock appends `-v1:0`.
    if let Some((name, _)) = release.split_once('@') {
        release = name;
    }
    if let Some((name, version)) = release.rsplit_once("-v")
        && version
            .split(':')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        release = name;
    }
    (release.len() > "claude-".len()).then(|| release.replace('.', "-"))
}

/// Whether a definition's `model` may leave out the provider: a family, or a
/// Claude release id such as `claude-sonnet-5-5`, as Claude Code writes them.
pub fn unqualified_model(value: &str) -> bool {
    MODEL_FAMILIES.contains(&value) || (!value.contains('/') && claude_release(value).is_some())
}
pub fn execution(
    front: &BTreeMap<String, String>,
    path: &Path,
    plugin_root: Option<&Path>,
) -> DefinitionExecution {
    DefinitionExecution {
        user_invocable: front.get("user-invocable").is_none_or(|v| v != "false"),
        disable_model_invocation: front
            .get("disable-model-invocation")
            .is_some_and(|v| v == "true"),
        // `subtask: true` is the Haarness spelling of Claude's `context: fork`.
        fork: front.get("context").is_some_and(|v| v == "fork")
            || front.get("subtask").is_some_and(|v| v == "true"),
        claude_arguments: front
            .get("haarness-claude-arguments")
            .is_some_and(|v| v == "true")
            || plugin_root.is_some()
            || path.file_name().is_some_and(|s| s == "SKILL.md"),
        allowed_tools: list(front.get("allowed-tools")),
        disallowed_tools: list(front.get("disallowed-tools")),
        arguments: list(front.get("arguments")),
        path: Some(path.to_string_lossy().into_owned()),
        directory: front
            .get("haarness-definition-directory")
            .cloned()
            .or_else(|| path.parent().map(|p| p.to_string_lossy().into_owned())),
        plugin_root: plugin_root
            .map(|p| p.to_string_lossy().into_owned())
            .or_else(|| front.get("haarness-plugin-root").cloned()),
        agent: front.get("agent").map(|v| match v.as_str() {
            "Explore" => "explore".into(),
            "Plan" => "plan".into(),
            "general-purpose" => "general".into(),
            _ => v.clone(),
        }),
        model: front.get("model").cloned(),
    }
}
pub fn plugin_root(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|p| p.join(".claude-plugin/plugin.json").is_file())
        .map(Path::to_path_buf)
}
pub fn plugin_name(root: &Path) -> Option<String> {
    std::fs::read_to_string(root.join(".claude-plugin/plugin.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.get("name").and_then(Value::as_str).map(str::to_owned))
        .or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()))
}
/// Custom paths supplement defaults and must resolve within the plugin bundle.
pub fn component_paths(root: &Path, kind: &str) -> Vec<PathBuf> {
    let mut paths = vec![root.join(kind)];
    let manifest = std::fs::read_to_string(root.join(".claude-plugin/plugin.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or(Value::Null);
    let entries = match manifest.get(kind) {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => vec![],
    };
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    for entry in entries {
        let path = root.join(&entry);
        if Path::new(&entry).is_absolute()
            || Path::new(&entry)
                .components()
                .any(|p| matches!(p, std::path::Component::ParentDir))
        {
            continue;
        }
        if let Ok(path) = path.canonicalize() {
            if path.starts_with(&canonical)
                && !paths
                    .iter()
                    .any(|p| p.canonicalize().ok().as_ref() == Some(&path))
            {
                paths.push(path);
            }
        }
    }
    paths
}
pub fn skill_name(front: &BTreeMap<String, String>, path: &Path) -> String {
    front
        .get("name")
        .filter(|s| !s.is_empty() && !s.chars().any(char::is_whitespace))
        .cloned()
        .unwrap_or_else(|| {
            path.parent()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
}
pub fn tool_names(name: &str) -> Vec<String> {
    let base = name.split('(').next().unwrap_or(name).trim();
    match base {
        "Read" => vec!["read"],
        "Write" => vec!["write"],
        "Edit" | "MultiEdit" | "NotebookEdit" => vec!["edit", "apply_patch"],
        "Bash" => vec!["bash"],
        "Grep" => vec!["grep"],
        "Glob" => vec!["glob"],
        "LS" => vec!["list"],
        "Task" | "Agent" => vec!["task"],
        "Skill" => vec!["skill"],
        "WebFetch" => vec!["webfetch"],
        "WebSearch" => vec!["web_search"],
        "TodoWrite" => vec!["todowrite"],
        other => {
            return vec![
                if other.starts_with("mcp__") && other.matches("__").count() == 1 {
                    format!("{other}__*")
                } else {
                    other.into()
                },
            ];
        }
    }
    .into_iter()
    .map(str::to_owned)
    .collect()
}
pub fn tool_override(tools: &std::collections::HashMap<String, bool>, name: &str) -> Option<bool> {
    if let Some(v) = tools.get(name) {
        return Some(*v);
    }
    tools
        .iter()
        .filter(|(p, _)| p.ends_with('*') && name.starts_with(p.trim_end_matches('*')))
        .max_by_key(|(p, _)| p.len())
        .map(|(_, v)| *v)
}

pub fn arguments(template: &str, input: &str, claude: bool, shell: bool) -> String {
    expand_arguments(template, input, claude, shell, &[])
}
fn expand_arguments(
    template: &str,
    input: &str,
    claude: bool,
    shell: bool,
    names: &[String],
) -> String {
    let words = shell_words::split(input)
        .unwrap_or_else(|_| input.split_whitespace().map(str::to_owned).collect());
    let named = names
        .iter()
        .map(|n| regex::escape(n))
        .collect::<Vec<_>>()
        .join("|");
    let expression = if named.is_empty() {
        r"\$ARGUMENTS(?:\[(\d+)\])?|\$(\d+)".into()
    } else {
        format!(r"\$ARGUMENTS(?:\[(\d+)\])?|\$(\d+)|\$({named})\b")
    };
    let tokens = regex::Regex::new(&expression).expect("escaped argument names");
    let highest = tokens
        .captures_iter(template)
        .filter_map(|c| c.get(2)?.as_str().parse::<usize>().ok())
        .max();
    let mut found = false;
    let mut result = String::new();
    let mut last = 0;
    for c in tokens.captures_iter(template) {
        let start = c.get(0).unwrap().start();
        if claude && template[..start].ends_with('\\') && !template[..start].ends_with("\\\\") {
            result.push_str(&template[last..start - 1]);
            result.push_str(&c[0]);
            last = c.get(0).unwrap().end();
            continue;
        }
        result.push_str(&template[last..start]);
        last = c.get(0).unwrap().end();
        found = true;
        let value = if let Some(index) = c.get(1) {
            words
                .get(index.as_str().parse::<usize>().unwrap_or(usize::MAX))
                .cloned()
                .unwrap_or_else(|| c[0].to_owned())
        } else if let Some(index) = c.get(2) {
            let index = index.as_str().parse::<usize>().unwrap_or(usize::MAX);
            if claude {
                words.get(index).cloned().unwrap_or_else(|| c[0].to_owned())
            } else if index == 0 {
                c[0].to_owned()
            } else if Some(index) == highest {
                words
                    .get(index - 1..)
                    .map(|w| w.join(" "))
                    .unwrap_or_default()
            } else {
                words.get(index - 1).cloned().unwrap_or_default()
            }
        } else if let Some(name) = c.get(3) {
            names
                .iter()
                .position(|n| n == name.as_str())
                .and_then(|i| words.get(i))
                .cloned()
                .unwrap_or_default()
        } else {
            input.to_owned()
        };
        result.push_str(&if shell {
            shell_words::quote(&value).into_owned()
        } else {
            value
        });
    }
    result.push_str(&template[last..]);
    if !found && !shell && !input.trim().is_empty() {
        format!("{result}\n\n{input}")
    } else {
        result
    }
}

/// Shell snippets are parsed from trusted definition text before argument
/// substitution, so an argument containing !`...` cannot introduce a command.
pub async fn prepare(
    template: &str,
    input: &str,
    execution: &DefinitionExecution,
    session: &str,
    invoker: &dyn crate::ToolInvoker,
) -> CoreResult<String> {
    let project_template = template.replace(
        "${CLAUDE_PROJECT_DIR}",
        &invoker.workspace().unwrap_or_default(),
    );
    let template = project_template.as_str();
    let snippets = regex::Regex::new(r"(?m)(?:^|[ \t\r\n])!`([^`]+)`").expect("constant regex");
    let mut prepared = String::new();
    let mut last = 0;
    for capture in snippets.captures_iter(template) {
        let whole = capture.get(0).expect("whole match");
        prepared.push_str(&substitutions(
            &template[last..whole.start()],
            input,
            execution,
            session,
            false,
            false,
        ));
        prepared.push_str(whole.as_str().split_once("!`").unwrap().0);
        let command = substitutions(&capture[1], input, execution, session, true, false);
        let result = invoker.definition_shell(command).await?;
        if result
            .metadata
            .get("exit")
            .and_then(Value::as_i64)
            .is_some_and(|code| code != 0)
        {
            return Err(CoreError::Tool(format!(
                "Definition context command failed: {}",
                result.text
            )));
        }
        prepared.push_str(
            result
                .metadata
                .get("stdout")
                .and_then(Value::as_str)
                .unwrap_or(&result.text)
                .trim_end(),
        );
        last = whole.end();
    }
    prepared.push_str(&substitutions(
        &template[last..],
        input,
        execution,
        session,
        false,
        false,
    ));
    // Append implicit arguments once, not once per text segment.
    if !template.contains("$ARGUMENTS")
        && !regex::Regex::new(r"\$\d+").unwrap().is_match(template)
        && !execution
            .arguments
            .iter()
            .any(|n| template.contains(&format!("${n}")))
        && !input.trim().is_empty()
    {
        prepared.push_str("\n\n");
        prepared.push_str(input);
    }
    Ok(expand_references(&prepared, invoker))
}

/// Bounds for `@path` expansion in command templates.
const REFERENCE_FILE_LIMIT: usize = 20;
const REFERENCE_FILE_BYTES: usize = 128 * 1024;
const REFERENCE_DIR_ENTRIES: usize = 200;

/// `@path` tokens in a prepared template become bounded snapshots of
/// workspace files, one-level directory listings, or delegation hints for
/// subagent-mode agents, appended after the text. Tokens inside code stay
/// literal; paths outside the
/// workspace are ignored unless written with an explicit `~/`.
fn expand_references(text: &str, invoker: &dyn crate::ToolInvoker) -> String {
    let references = crate::instructions::references(text);
    if references.is_empty() {
        return text.to_owned();
    }
    let workspace = invoker
        .workspace()
        .map(PathBuf::from)
        .and_then(|path| path.canonicalize().ok());
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let agents = invoker.delegable_agents();
    let mut seen = std::collections::HashSet::new();
    let mut blocks = Vec::new();
    let mut files = 0;
    for reference in references {
        let name = reference.path;
        if !seen.insert(name.clone()) {
            continue;
        }
        let target = reference_target(&name, workspace.as_deref(), home.as_deref());
        let Some(target) = target.filter(|target| target.exists()) else {
            if agents.iter().any(|agent| agent == &name) {
                blocks.push(delegation_hint(&name));
            }
            continue;
        };
        if files >= REFERENCE_FILE_LIMIT {
            break;
        }
        files += 1;
        if target.is_dir() {
            blocks.push(directory_listing(&name, &target));
            continue;
        }
        let size = std::fs::metadata(&target)
            .map(|m| m.len())
            .unwrap_or(u64::MAX);
        if size > REFERENCE_FILE_BYTES as u64 {
            blocks.push(format!("[File context omitted: {name} exceeds 128 KiB]"));
            continue;
        }
        match std::fs::read_to_string(&target) {
            Ok(content) if !content.contains('\0') => blocks.push(file_snapshot(&name, &content)),
            _ => blocks.push(format!("[File context omitted: {name} is not a text file]")),
        }
    }
    if blocks.is_empty() {
        return text.to_owned();
    }
    format!("{text}\n\n{}", blocks.join("\n\n"))
}

fn reference_target(name: &str, workspace: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(rest) = name.strip_prefix("~/") {
        return home.map(|home| home.join(rest));
    }
    let workspace = workspace?;
    let candidate = if Path::new(name).is_absolute() {
        PathBuf::from(name)
    } else {
        workspace.join(name)
    };
    let canonical = candidate.canonicalize().ok()?;
    canonical.starts_with(workspace).then_some(canonical)
}

/// Same shape as the TUI/desktop file mention snapshot.
fn file_snapshot(label: &str, content: &str) -> String {
    let fence = "`".repeat(
        content
            .split(|c| c != '`')
            .map(str::len)
            .max()
            .unwrap_or(0)
            .max(2)
            + 1,
    );
    format!(
        "<details>\n<summary>File context: {} (snapshot)</summary>\n\n{fence}\n{content}\n{fence}\n\n</details>",
        escape_label(label)
    )
}

fn directory_listing(label: &str, directory: &Path) -> String {
    let mut entries = std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| {
            let mut name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                name.push('/');
            }
            name
        })
        .collect::<Vec<_>>();
    entries.sort();
    let total = entries.len();
    entries.truncate(REFERENCE_DIR_ENTRIES);
    let mut listing = entries.join("\n");
    if total > REFERENCE_DIR_ENTRIES {
        let _ = write!(
            listing,
            "\n… {} more entries",
            total - REFERENCE_DIR_ENTRIES
        );
    }
    format!(
        "<details>\n<summary>Directory listing: {}</summary>\n\n{listing}\n\n</details>",
        escape_label(label)
    )
}

fn escape_label(label: &str) -> String {
    label
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The delegation text the TUI and desktop attach for an `@agent` mention.
fn delegation_hint(agent: &str) -> String {
    format!(
        "Delegate the relevant work from this message and attached context using the task tool with agent {}. Keep normal permissions in effect.",
        serde_json::to_string(agent).unwrap_or_default()
    )
}
fn substitutions(
    template: &str,
    input: &str,
    execution: &DefinitionExecution,
    session: &str,
    shell: bool,
    _append: bool,
) -> String {
    // Do not let supplied arguments be reinterpreted as template substitutions.
    let mut template = template.replace("${CLAUDE_SESSION_ID}", session);
    if let Some(directory) = &execution.directory {
        template = template.replace("${CLAUDE_SKILL_DIR}", directory);
    }
    if let Some(root) = &execution.plugin_root {
        for key in [
            "CLAUDE_PLUGIN_ROOT",
            "HAARNESS_PLUGIN_ROOT",
            "OPENCODE_PLUGIN_ROOT",
        ] {
            template = template.replace(&format!("${{{key}}}"), root);
        }
    }
    // arguments() has legacy implicit-tail behavior; suppress it for segments.
    if regex::Regex::new(r"\$ARGUMENTS|\$\d+")
        .unwrap()
        .is_match(&template)
        || execution
            .arguments
            .iter()
            .any(|n| template.contains(&format!("${n}")))
    {
        expand_arguments(
            &template,
            input,
            execution.claude_arguments,
            shell,
            &execution.arguments,
        )
    } else {
        template
    }
}
pub fn approval_rules(tools: &[String]) -> Vec<crate::PermissionRule> {
    tools
        .iter()
        .flat_map(|entry| {
            let pattern = entry
                .split_once('(')
                .map(|(_, p)| p.trim_end_matches(')').replace(":*", " *"))
                .unwrap_or_else(|| "*".into());
            tool_names(entry)
                .into_iter()
                .map(move |tool| crate::PermissionRule {
                    pattern: format!("{tool}:{pattern}"),
                    decision: crate::PermissionDecision::Allow,
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn yaml_preserves_structures_and_multiline_metadata() {
        let (front, body) = markdown("---\r\nname: example\r\ndescription: >-\r\n  First line\r\n  second line\r\ntools:\r\n  - Read\r\n  - Grep\r\nmetadata:\r\n  author: test\r\n---\r\nBody\r\n").unwrap();
        assert_eq!(front["description"], "First line second line");
        assert_eq!(list(front.get("tools")), vec!["Read", "Grep"]);
        assert_eq!(front["metadata"], "{\"author\":\"test\"}");
        assert_eq!(body, "Body\r\n");
        assert!(markdown("---\ntools: [Read\n---\nbody").is_err());
    }
    #[test]
    fn claude_arguments_are_zero_based_quoted_and_non_recursive() {
        assert_eq!(
            arguments("$0/$1/$ARGUMENTS[2]", "one 'two words' three", true, false),
            "one/two words/three"
        );
        assert_eq!(arguments("$0 $ARGUMENTS[0]", "$1", true, false), "$1 $1");
        assert_eq!(
            arguments("$1 $2", "one two three", false, false),
            "one two three"
        );
        assert_eq!(
            arguments("$ARGUMENTS[10]", "one", true, false),
            "$ARGUMENTS[10]"
        );
        assert_eq!(
            list(Some(&"Read Bash(git status *) Grep".into())),
            vec!["Read", "Bash(git status *)", "Grep"]
        );
    }
    #[test]
    fn legacy_examples_do_not_discard_policy_fields() {
        let (front, _) = markdown("---\nname: test\ndescription: Use this. Examples:\n\n<example>\nuser: help\n</example>\nmodel: inherit\ntools: [Read, Grep]\n---\nBody").unwrap();
        assert_eq!(front["model"], "inherit");
        assert_eq!(list(front.get("tools")), vec!["Read", "Grep"]);
        assert!(markdown("---\ndescription: Test\ntools: [Read\n---\nBody").is_err());
        assert_eq!(
            expand_arguments(
                "$file/$target/$missing",
                "'two words' last",
                true,
                false,
                &["file".into(), "target".into(), "missing".into()]
            ),
            "two words/last/"
        );
        assert_eq!(
            arguments(r"\$0 $0 \\$1", "one two", true, false),
            r"$0 one \\two"
        );
        assert_eq!(arguments("$0", r"'\$1'", true, false), r"\$1");
    }
    #[test]
    fn claude_code_text_values_load_without_coercing_collections() {
        let other = "disable-model-invocation: true\r\ntags: [a, b]\r\nmetadata: {\r\n  key: value\r\n}\r\ncount: 3 # note: keep numeric\r\ntime: 12:30\r\nurl: https://example.test:8443/path\r\nnotes: |-\r\n  Keep: this text\r\nsummary: first line\r\n  second line\r\n";
        let text = format!(
            "---\r\nname: deploy\r\ndescription: TRIGGER: when asked # internal\r\nargument-hint: [filename] [format]\r\nallowed-tools: Bash(git commit: *), Read\r\n{other}---\r\nBody: $& stays\r\n"
        );
        let (front, body) = markdown(&text).unwrap();
        assert_eq!(front["name"], "deploy");
        assert_eq!(front["description"], "TRIGGER: when asked");
        assert_eq!(front["argument-hint"], "[filename] [format]");
        let execution = execution(&front, Path::new("/tmp/deploy.md"), None);
        assert_eq!(execution.allowed_tools, vec!["Bash(git commit: *)", "Read"]);
        assert!(execution.disable_model_invocation);
        // Lines YAML accepts keep exactly the values they have without the repair.
        let (plain, _) = markdown(&format!("---\r\n{other}---\r\n")).unwrap();
        for (key, value) in &plain {
            assert_eq!(front.get(key), Some(value), "{key}");
        }
        assert_eq!(list(front.get("tags")), vec!["a", "b"]);
        assert_eq!(front["notes"], "Keep: this text");
        assert_eq!(body, "Body: $& stays\r\n");
        // A value YAML reads as a list, map or alias is never turned into text,
        // so a malformed policy field still fails the file.
        for value in ["[Bash(git commit: *)]", "[Read,", "[Read] [Grep]"] {
            let text = format!(
                "---\nname: malformed\ndescription: Use: on demand\nallowed-tools: {value}\n---\nBody"
            );
            assert!(markdown(&text).is_err(), "{value}");
        }
        assert!(
            markdown("---\nname: broken\nallowed-tools:\n  - Bash(git commit: *)\n---\nBody")
                .is_err()
        );
        // Writers quote whatever only the repair would read back.
        for value in ["anthropic/claude-sonnet-5-5", "true", "12:30", "Fix typo"] {
            assert!(plain_value(value), "{value}");
        }
        for value in [
            "Fix: typo",
            "[filename] [format]",
            "[file]",
            "a # b",
            "two\nlines",
        ] {
            assert!(!plain_value(value), "{value}");
        }
    }
    #[test]
    fn tool_masks_handle_defaults_and_mcp_patterns() {
        let map = std::collections::HashMap::from([
            ("*".into(), false),
            ("read".into(), true),
            ("mcp__docs__*".into(), true),
            ("mcp__docs__write".into(), false),
        ]);
        assert_eq!(tool_override(&map, "read"), Some(true));
        assert_eq!(tool_override(&map, "write"), Some(false));
        assert_eq!(tool_override(&map, "mcp__docs__search"), Some(true));
        assert_eq!(tool_override(&map, "mcp__docs__write"), Some(false));
    }

    #[test]
    fn subtask_frontmatter_is_an_alias_of_context_fork() {
        let (front, _) = markdown("---\ndescription: Fork\nsubtask: true\n---\nBody").unwrap();
        assert!(execution(&front, Path::new("/tmp/fork.md"), None).fork);
        let (front, _) = markdown("---\ncontext: fork\n---\nBody").unwrap();
        assert!(execution(&front, Path::new("/tmp/fork.md"), None).fork);
        let (front, _) = markdown("---\nsubtask: false\n---\nBody").unwrap();
        assert!(!execution(&front, Path::new("/tmp/fork.md"), None).fork);
    }

    struct StubInvoker {
        workspace: PathBuf,
        agents: Vec<String>,
    }
    #[async_trait::async_trait]
    impl crate::ToolInvoker for StubInvoker {
        fn workspace(&self) -> Option<String> {
            Some(self.workspace.display().to_string())
        }
        fn delegable_agents(&self) -> Vec<String> {
            self.agents.clone()
        }
        async fn invoke(&self, _: &str, _: Value) -> CoreResult<crate::ToolOutput> {
            Err(CoreError::Tool("unused".into()))
        }
        fn side_effectful_for(&self, _: &str, _: &Value) -> CoreResult<bool> {
            Ok(false)
        }
    }

    #[tokio::test]
    async fn template_references_expand_files_directories_and_agents() {
        let root = std::env::temp_dir().join(format!("haarness-refs-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(root.join("src/nested")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("src/big.bin"), vec![0_u8; 10]).unwrap();
        std::fs::write(root.join("notes.md"), "note body").unwrap();
        let invoker = StubInvoker {
            workspace: root.clone(),
            agents: vec!["reviewer".into()],
        };
        let execution = DefinitionExecution::default();
        let text = prepare(
            "Review @src/lib.rs and @src/ then @reviewer; ignore `@notes.md`, @missing.md, @src/big.bin and @../escape.md",
            "",
            &execution,
            "session",
            &invoker,
        )
        .await
        .unwrap();
        assert!(text.starts_with("Review @src/lib.rs and @src/"), "{text}");
        assert!(text.contains("<summary>File context: src/lib.rs (snapshot)</summary>"));
        assert!(text.contains("fn main() {}"));
        assert!(text.contains("<summary>Directory listing: src/</summary>"));
        assert!(text.contains("\nlib.rs\n") && text.contains("nested/"));
        assert!(text.contains("using the task tool with agent \"reviewer\"."));
        assert!(!text.contains("note body"));
        assert!(text.contains("[File context omitted: src/big.bin is not a text file]"));
        assert!(!text.contains("escape.md is"));
        assert_eq!(text.matches("File context:").count(), 1);

        // User arguments participate after substitution; unknown names are inert.
        let text = prepare(
            "Look at $ARGUMENTS",
            "@notes.md @nobody",
            &execution,
            "s",
            &invoker,
        )
        .await
        .unwrap();
        assert!(text.contains("note body"));
        assert!(!text.contains("task tool"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
