//! Shared model-call validation and conservative name recovery.

use jsonschema::{
    JsonType,
    error::{TypeKind, ValidationErrorKind},
};
use serde_json::Value;

use crate::{CoreError, CoreResult, ToolDefinition};

pub(crate) fn resolve_name(name: &str, names: impl IntoIterator<Item = String>) -> String {
    let mut names: Vec<_> = names.into_iter().collect();
    names.sort();
    names.dedup();
    if names.iter().any(|registered| registered == name) {
        return name.to_owned();
    }
    // A name the model wrote as providers were sent the tool (a `batch` or
    // Code Mode step, a workflow it wrote): the tool's own name, before any
    // repair of letter case, which could reach another tool instead
    // (`mcp__Google_Drive__search` is `Google Drive`'s, not `google_drive`'s).
    let wire = crate::wire_names::WireNames::new(names.iter().map(String::as_str));
    let own = wire.own(name);
    if own != name {
        return own.to_owned();
    }
    let local = name
        .get(..10)
        .filter(|prefix| prefix.eq_ignore_ascii_case("functions."))
        .map(|_| &name[10..]);
    for candidate in std::iter::once(name).chain(local) {
        if names.iter().any(|registered| registered == candidate) {
            return candidate.to_owned();
        }
        let mut matches = names
            .iter()
            .filter(|registered| registered.eq_ignore_ascii_case(candidate));
        if let Some(found) = matches.next() {
            return if matches.next().is_none() {
                found.clone()
            } else {
                name.to_owned()
            };
        }
    }
    name.to_owned()
}

pub(crate) fn unknown_tool(name: &str, names: impl IntoIterator<Item = String>) -> CoreError {
    let mut names: Vec<_> = names.into_iter().collect();
    names.sort();
    names.dedup();
    // Bound feedback, never the executable catalog.
    let more = if names.len() > 12 {
        " (showing 12)"
    } else {
        ""
    };
    let available = if names.is_empty() {
        "No tools are currently available.".to_owned()
    } else {
        format!(
            "Available tools{more}: {}.",
            names
                .iter()
                .take(12)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    CoreError::Tool(format!(
        "unknown tool: {name}. Use an exact available tool name. {available}"
    ))
}

/// The key under which a tool call keeps arguments that were not a JSON
/// object, and the one naming what was wrong with them.
const UNPARSED: &str = "_unparsed_arguments";
const UNPARSED_REASON: &str = "_unparsed_reason";

/// A model's tool-call arguments as the provider streamed them. Text that is
/// not a JSON object (a stray escape, a cut-off string) is kept, with why it
/// was refused, in an object the runtime answers with an error instead of
/// running the tool: the model can correct the call, the turn goes on, and
/// the call still goes back to any provider as a JSON object. No arguments
/// at all are an empty object.
#[must_use]
pub fn tool_call_arguments(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return Value::Object(serde_json::Map::new());
    }
    let parsed = serde_json::from_str::<Value>(raw);
    // A Windows path written into the JSON with single backslashes
    // (`"path": "C:\Users\sam\src"`) holds escapes JSON does not have:
    // each such backslash stands for itself.
    if parsed.is_err()
        && let Some(value) = with_stray_backslashes_escaped(raw)
            .and_then(|repaired| serde_json::from_str::<Value>(&repaired).ok())
            .filter(Value::is_object)
    {
        return value;
    }
    let reason = match parsed {
        Ok(value) if value.is_object() => return value,
        // Some servers encode the arguments twice, as a JSON string that
        // holds the object.
        Ok(Value::String(inner)) => match serde_json::from_str::<Value>(&inner) {
            Ok(value) if value.is_object() => return value,
            _ => "they are not a JSON object".to_owned(),
        },
        Ok(_) => "they are not a JSON object".to_owned(),
        Err(error) => format!("they are not valid JSON: {error}"),
    };
    serde_json::json!({ UNPARSED: raw, UNPARSED_REASON: reason })
}

/// `raw` with each JSON string that holds a backslash starting no escape
/// JSON has (`\U`, `\s`, `\.`, or `\u` without four hex digits) read as
/// written raw, or `None` when no string does. A string written raw is a
/// path or pattern whose every backslash stands for itself, `\n` and `\t`
/// among them (`C:\Users\sam\new` is no line break), so only a backslash
/// before a quote or another backslash keeps its JSON meaning there. Other
/// strings keep their escapes.
fn with_stray_backslashes_escaped(raw: &str) -> Option<String> {
    let mut repaired = String::with_capacity(raw.len() + 8);
    let mut changed = false;
    let mut rest = raw;
    while let Some(open) = rest.find('"') {
        repaired.push_str(&rest[..=open]);
        let body = &rest[open + 1..];
        // The string ends at the first quote no backslash escapes.
        let mut end = None;
        let mut stray = false;
        let mut chars = body.char_indices();
        while let Some((at, c)) = chars.next() {
            match c {
                '"' => {
                    end = Some(at);
                    break;
                }
                '\\' => {
                    let next = body[at + 1..].chars().next();
                    stray |= match next {
                        Some('"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't') => false,
                        Some('u') => {
                            body[at + 2..]
                                .chars()
                                .take(4)
                                .filter(char::is_ascii_hexdigit)
                                .count()
                                != 4
                        }
                        _ => true,
                    };
                    if matches!(next, Some('"' | '\\')) {
                        chars.next();
                    }
                }
                _ => {}
            }
        }
        // A string left open is past repairing: JSON refuses it anyway.
        let Some(end) = end else {
            repaired.push_str(body);
            return changed.then_some(repaired);
        };
        let text = &body[..end];
        if stray {
            let mut chars = text.chars().peekable();
            while let Some(c) = chars.next() {
                if c != '\\' {
                    repaired.push(c);
                } else if let Some(next) = chars.next_if(|next| matches!(next, '"' | '\\')) {
                    repaired.push(c);
                    repaired.push(next);
                } else {
                    repaired.push_str("\\\\");
                }
            }
            changed = true;
        } else {
            repaired.push_str(text);
        }
        repaired.push('"');
        rest = &body[end + 1..];
    }
    repaired.push_str(rest);
    changed.then_some(repaired)
}

/// The error a call with arguments [`tool_call_arguments`] could not read
/// gets in place of running.
pub(crate) fn unparsed_arguments(name: &str, arguments: &Value) -> Option<CoreError> {
    let object = arguments.as_object()?;
    if object.len() != 2 || !object.get(UNPARSED)?.is_string() {
        return None;
    }
    let reason = object.get(UNPARSED_REASON)?.as_str()?;
    Some(CoreError::Invalid(format!(
        "The {name} tool did not run because its arguments could not be read: {reason}. Call the same tool again with its arguments as one valid JSON object."
    )))
}

pub(crate) fn validate_arguments(tool: &ToolDefinition, arguments: &Value) -> CoreResult<()> {
    if let Some(error) = unparsed_arguments(&tool.name, arguments) {
        return Err(error);
    }
    // No HTTP/file resolver features are enabled. A broken tool schema is a
    // configuration error, not permission to execute an unvalidated payload.
    let validator = jsonschema::validator_for(&tool.input_schema).map_err(|error| {
        CoreError::Tool(format!(
            "tool {} has an invalid input schema at {} ({}). The tool definition must be fixed before retrying.",
            tool.name, error.schema_path(), error.kind().keyword()
        ))
    })?;
    let mut errors = validator.iter_errors(arguments);
    let mut issues = Vec::new();
    for error in errors.by_ref().take(8) {
        let mut path = argument_path(error.instance_path().as_str(), arguments);
        let message = match error.kind() {
            ValidationErrorKind::Required { property } => {
                if let Some(property) = property.as_str() {
                    append_property(&mut path, property);
                }
                "required field is missing".into()
            }
            ValidationErrorKind::Type { kind } => match kind {
                TypeKind::Single(kind) => format!("expected {kind}"),
                TypeKind::Multiple(kinds) => format!("expected one of {kinds:?}"),
            },
            ValidationErrorKind::Enum { options } => format!("expected one of {options}"),
            ValidationErrorKind::Constant { expected_value } => {
                format!("expected {expected_value}")
            }
            ValidationErrorKind::AdditionalProperties { unexpected }
            | ValidationErrorKind::UnevaluatedProperties { unexpected } => {
                format!("unexpected fields: {}", unexpected.join(", "))
            }
            ValidationErrorKind::MinLength { limit } => {
                format!("expected at least {limit} characters")
            }
            ValidationErrorKind::MaxLength { limit } => {
                format!("expected at most {limit} characters")
            }
            ValidationErrorKind::MinItems { limit } => format!("expected at least {limit} items"),
            ValidationErrorKind::MaxItems { limit } => format!("expected at most {limit} items"),
            ValidationErrorKind::Minimum { limit } => format!("expected a value >= {limit}"),
            ValidationErrorKind::Maximum { limit } => format!("expected a value <= {limit}"),
            ValidationErrorKind::Pattern { pattern } => format!("must match pattern {pattern}"),
            ValidationErrorKind::AnyOf { .. } => {
                "must match at least one allowed argument shape".into()
            }
            ValidationErrorKind::OneOfNotValid { .. }
            | ValidationErrorKind::OneOfMultipleValid { .. } => {
                "must match exactly one allowed argument shape".into()
            }
            kind => format!("does not satisfy {}", kind.keyword()),
        };
        let path = if path.is_empty() { "<root>" } else { &path };
        // Do not echo rejected values (file contents, credentials, etc.).
        issues.push(format!("- {path}: {message}"));
    }
    if issues.is_empty() {
        return Ok(());
    }
    if errors.next().is_some() {
        issues.push("- Additional schema issues omitted; fix these fields first.".into());
    }
    Err(CoreError::Invalid(format!(
        "The {} tool was called with invalid arguments. Fix the input and retry the same tool.\n{}",
        tool.name,
        issues.join("\n")
    )))
}

/// Arguments in which each value that failed the schema only for being a
/// string where the schema wants a number, an integer, a boolean or null,
/// and that is exactly one written out (Llama 3.1 sends `"timeout": "0"` and
/// `"run_in_background": "false"`), or where it wants an array or an object
/// and the string is the JSON of one (Llama 3.1 sends multiedit's `edits` as
/// the text `"[{\"old\": …}]"`), is that value, when every fault was such
/// a string and the arguments then pass. `None` otherwise: valid arguments,
/// any other fault, and a string the schema takes as a string are never
/// changed. In `anyOf` or `oneOf` the value is changed only when exactly one
/// alternative takes it. Nested objects and arrays are read the same way.
pub(crate) fn coerced_arguments(tool: &ToolDefinition, arguments: &Value) -> Option<Value> {
    let validator = jsonschema::validator_for(&tool.input_schema).ok()?;
    let coerced = coerced_once(tool, &validator, arguments)?;
    if validator.is_valid(&coerced) {
        return Some(coerced);
    }
    // An array or object read from its text may hold scalars written as
    // strings too (`"replaceAll": "false"`): read once more.
    coerced_once(tool, &validator, &coerced).filter(|coerced| validator.is_valid(coerced))
}

/// One reading of [`coerced_arguments`]: every fault's string as its value,
/// or `None` when a fault is anything else or nothing changed.
fn coerced_once(
    tool: &ToolDefinition,
    validator: &jsonschema::Validator,
    arguments: &Value,
) -> Option<Value> {
    let mut coerced = arguments.clone();
    let mut changed = false;
    for error in validator.iter_errors(arguments) {
        let Value::String(text) = error.instance().as_ref() else {
            return None;
        };
        let value = match error.kind() {
            ValidationErrorKind::Type { kind } => {
                let wanted: Vec<JsonType> = match kind {
                    TypeKind::Single(kind) => vec![*kind],
                    TypeKind::Multiple(kinds) => kinds.iter().collect(),
                };
                written_scalar(text)
                    .or_else(|| written_structure(text))
                    .filter(|value| {
                        wanted.iter().any(|kind| match (kind, value) {
                            (JsonType::Boolean, Value::Bool(_))
                            | (JsonType::Null, Value::Null)
                            | (JsonType::Number, Value::Number(_))
                            | (JsonType::Array, Value::Array(_))
                            | (JsonType::Object, Value::Object(_)) => true,
                            (JsonType::Integer, Value::Number(number)) => {
                                number.is_i64() || number.is_u64()
                            }
                            _ => false,
                        })
                    })
            }
            ValidationErrorKind::AnyOf { .. } | ValidationErrorKind::OneOfNotValid { .. } => {
                let alternatives = tool
                    .input_schema
                    .pointer(error.schema_path().as_str())?
                    .as_array()?;
                let value = written_scalar(text)?;
                let mut taking = 0;
                for alternative in alternatives {
                    // An alternative read alone may lean on the rest of the
                    // schema (`$ref`): one that can't be read alone stops it.
                    if jsonschema::validator_for(alternative)
                        .ok()?
                        .is_valid(&value)
                    {
                        taking += 1;
                    }
                }
                (taking == 1).then_some(value)
            }
            _ => None,
        }?;
        *coerced.pointer_mut(error.instance_path().as_str())? = value;
        changed = true;
    }
    changed.then_some(coerced)
}

/// Field names other coding agents give their tools, and the name each has
/// here: models trained on their transcripts call `edit` with `file_path`,
/// `old_string` and `new_string`, and `bash` with a `cwd`.
const FIELD_ALIASES: [(&str, &str); 11] = [
    ("file_path", "path"),
    ("filePath", "path"),
    ("filepath", "path"),
    ("old_string", "old"),
    ("oldString", "old"),
    ("old_str", "old"),
    ("new_string", "new"),
    ("newString", "new"),
    ("new_str", "new"),
    ("replace_all", "replaceAll"),
    ("cwd", "workdir"),
];

/// Arguments that pass once the fields another coding agent's tools name
/// their own way ([`FIELD_ALIASES`]) take the tool's names, and a
/// `description` of the call that the tool does not take is left out (Claude
/// Code's and OpenCode's shells ask for one; it describes the call and
/// changes nothing), reading scalars written as strings as
/// [`coerced_arguments`] does. `None` for valid arguments, any other fault, a
/// name the tool takes itself, and arguments that could not be read at all.
/// For every tool: a field renamed to its own name, or a caption left out,
/// changes nothing the call does.
pub(crate) fn with_field_aliases(tool: &ToolDefinition, arguments: &Value) -> Option<Value> {
    if unparsed_arguments(&tool.name, arguments).is_some() {
        return None;
    }
    let validator = jsonschema::validator_for(&tool.input_schema).ok()?;
    if validator.is_valid(arguments) {
        return None;
    }
    let properties = tool.input_schema.get("properties")?.as_object()?;
    let mut renamed = arguments.as_object()?.clone();
    let mut changed = false;
    for (alias, name) in FIELD_ALIASES {
        if properties.contains_key(alias)
            || !properties.contains_key(name)
            || renamed.contains_key(name)
        {
            continue;
        }
        if let Some(value) = renamed.remove(alias) {
            renamed.insert(name.to_owned(), value);
            changed = true;
        }
    }
    if !properties.contains_key("description")
        && renamed.get("description").is_some_and(Value::is_string)
    {
        renamed.remove("description");
        changed = true;
    }
    if !changed {
        return None;
    }
    let renamed = Value::Object(renamed);
    if validator.is_valid(&renamed) {
        return Some(renamed);
    }
    coerced_arguments(tool, &renamed)
}

/// Arguments with the top-level fields the tool does not take left out, and
/// the names of those fields, when leaving them out (and reading scalars
/// written as strings, as [`coerced_arguments`] does) is all the call needs
/// to pass: gpt-oss asks `read` for `line_start` and `line_end`, and `list`
/// for a `depth`. `None` for valid arguments, any other fault, a schema that
/// takes any field, and arguments that could not be read at all. For tools
/// that change nothing only: a field left out narrows nothing it changes.
pub(crate) fn without_unknown_fields(
    tool: &ToolDefinition,
    arguments: &Value,
) -> Option<(Value, Vec<String>)> {
    if unparsed_arguments(&tool.name, arguments).is_some()
        || tool.input_schema.get("additionalProperties") != Some(&Value::Bool(false))
    {
        return None;
    }
    let validator = jsonschema::validator_for(&tool.input_schema).ok()?;
    if validator.is_valid(arguments) {
        return None;
    }
    let properties = tool.input_schema.get("properties")?.as_object()?;
    let mut kept = arguments.as_object()?.clone();
    let unknown: Vec<String> = kept
        .keys()
        .filter(|field| !properties.contains_key(*field))
        .cloned()
        .collect();
    if unknown.is_empty() {
        return None;
    }
    kept.retain(|field, _| properties.contains_key(field));
    let kept = Value::Object(kept);
    if validator.is_valid(&kept) {
        return Some((kept, unknown));
    }
    coerced_arguments(tool, &kept).map(|coerced| (coerced, unknown))
}

/// The array or object a string is the JSON of, the spaces around it aside.
/// Line breaks and tabs inside its strings may be there as they are: Qwen3
/// Coder 30B writes the code it quotes in multiedit's `edits` that way,
/// which JSON itself does not allow.
fn written_structure(text: &str) -> Option<Value> {
    let text = text.trim();
    if !(text.starts_with('[') || text.starts_with('{')) {
        return None;
    }
    serde_json::from_str::<Value>(text)
        .ok()
        .or_else(|| serde_json::from_str::<Value>(&with_escaped_controls(text)?).ok())
        .filter(|value| value.is_array() || value.is_object())
}

/// `text` with each control character inside a JSON string written as its
/// escape, or `None` when it has none there.
fn with_escaped_controls(text: &str) -> Option<String> {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(text.len() + 16);
    let (mut in_string, mut escaped, mut changed) = (false, false, false);
    for character in text.chars() {
        let control = u32::from(character) < 0x20;
        if in_string && !escaped && control {
            changed = true;
            match character {
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                _ => {
                    let _ = write!(out, "\\u{:04x}", u32::from(character));
                }
            }
            continue;
        }
        if in_string && !escaped && character == '"' {
            in_string = false;
        } else if !in_string && character == '"' {
            in_string = true;
        }
        escaped = in_string && !escaped && character == '\\';
        out.push(character);
    }
    changed.then_some(out)
}

/// The JSON literal a string is, ignoring the spaces around it, when it is
/// one that a model writes for a scalar: `true`, `false` (or `True` and
/// `False`, as Python writes them), `null` or a number
/// in JSON's own form (`-3`, `1.5`, `1e5`; not `01`, `+1`, `.5`, `1.` or
/// `0x10`). An integer is one only within what a JSON integer holds.
fn written_scalar(text: &str) -> Option<Value> {
    let text = text.trim();
    match text {
        // And as Python writes them: Llama 3.1 sends `"run_in_background": "True"`.
        "true" | "True" => return Some(Value::Bool(true)),
        "false" | "False" => return Some(Value::Bool(false)),
        "null" => return Some(Value::Null),
        _ => {}
    }
    // JSON's number grammar, which `serde_json` follows exactly; anything
    // else (`NaN`, a word, an object) is no scalar.
    if !text.starts_with(|c: char| c == '-' || c.is_ascii_digit()) {
        return None;
    }
    match serde_json::from_str::<Value>(text).ok()? {
        Value::Number(number) => Some(Value::Number(number)),
        _ => None,
    }
}

fn argument_path(pointer: &str, arguments: &Value) -> String {
    let mut path = String::new();
    let mut value = Some(arguments);
    for part in pointer.split('/').skip(1) {
        let part = part.replace("~1", "/").replace("~0", "~");
        if value.is_some_and(Value::is_array) {
            use std::fmt::Write as _;
            let _ = write!(path, "[{part}]");
            value = part
                .parse::<usize>()
                .ok()
                .and_then(|index| value?.get(index));
        } else {
            append_property(&mut path, &part);
            value = value.and_then(|value| value.get(&part));
        }
    }
    path
}

fn append_property(path: &mut String, property: &str) {
    use std::fmt::Write as _;
    if !property.is_empty()
        && property.chars().enumerate().all(|(index, ch)| {
            ch == '_' || ch.is_ascii_alphabetic() || (index > 0 && ch.is_ascii_digit())
        })
    {
        if !path.is_empty() {
            path.push('.');
        }
        path.push_str(property);
    } else {
        let _ = write!(
            path,
            "[{}]",
            serde_json::to_string(property).expect("string is serializable")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn arguments_that_do_not_parse_are_kept_and_refused_without_running() {
        assert_eq!(
            tool_call_arguments(r#"{"path":"a.txt"}"#),
            json!({"path":"a.txt"})
        );
        assert_eq!(tool_call_arguments("  "), json!({}));
        // Encoded twice, as a JSON string that holds the object.
        assert_eq!(
            tool_call_arguments(r#""{\"path\":\"d.txt\"}""#),
            json!({"path":"d.txt"})
        );
        let broken = r#"{"command":"grep -r src"#;
        let kept = tool_call_arguments(broken);
        // Replayed to any provider, the call is still a JSON object, and it
        // still says what the model wrote.
        assert_eq!(kept[UNPARSED], broken);
        let tool = definition(json!({"type":"object","properties":{"command":{"type":"string"}}}));
        let error = validate_arguments(&tool, &kept).unwrap_err().to_string();
        assert!(error.contains("fixture tool did not run"), "{error}");
        assert!(
            error.contains("not valid JSON: EOF while parsing"),
            "{error}"
        );
        assert!(
            !error.contains("grep"),
            "the rejected text is not echoed: {error}"
        );
        let error = validate_arguments(&tool, &tool_call_arguments("[1,2]"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("not a JSON object"), "{error}");
        // A real argument of that name is an ordinary argument.
        assert!(unparsed_arguments("fixture", &json!({UNPARSED: "x"})).is_none());
    }

    #[test]
    fn backslashes_that_start_no_json_escape_stand_for_themselves() {
        // Windows paths and regular expressions written with single
        // backslashes.
        assert_eq!(
            tool_call_arguments(r#"{"path":"C:\Users\sam\src\main.py"}"#),
            json!({"path": r"C:\Users\sam\src\main.py"})
        );
        assert_eq!(
            tool_call_arguments(r#"{"command":"grep -r \. src","workdir":"D:\users\x"}"#),
            json!({"command": r"grep -r \. src", "workdir": r"D:\users\x"})
        );
        // Escapes JSON has keep their meaning in the other strings, beside
        // the repaired one.
        assert_eq!(
            tool_call_arguments(r#"{"content":"a\nb \u00e9 \"q\" \\","path":"C:\Temp\a.txt"}"#),
            json!({"content": "a\nb \u{e9} \"q\" \\", "path": r"C:\Temp\a.txt"})
        );
        // A string written raw is raw throughout: `\n` and `\t` in a path
        // are folders, not a line break that would split a command in two.
        assert_eq!(
            tool_call_arguments(r#"{"command":"dir C:\Users\sam\new\tests","path":"x\"y"}"#),
            json!({"command": r"dir C:\Users\sam\new\tests", "path": "x\"y"})
        );
        assert_eq!(
            tool_call_arguments(r#"{"path":"C:\\Users\sam\notes \"draft\".md"}"#),
            json!({"path": r#"C:\Users\sam\notes "draft".md"#})
        );
        // Outside a string a backslash is no path: still refused.
        assert!(tool_call_arguments(r#"{"a":1}\x"#)[UNPARSED].is_string());
    }

    fn edit_like() -> ToolDefinition {
        definition(json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "old": {"type": "string"},
                "new": {"type": "string"},
            },
            "required": ["path", "old", "new"],
            "additionalProperties": false,
        }))
    }

    #[test]
    fn fields_named_as_other_agents_name_them_take_the_tools_names() {
        assert_eq!(
            with_field_aliases(
                &edit_like(),
                &json!({"file_path": "a.txt", "old_string": "x", "new_string": "y"})
            ),
            Some(json!({"path": "a.txt", "old": "x", "new": "y"}))
        );
        assert_eq!(
            with_field_aliases(
                &edit_like(),
                &json!({"filePath": "a.txt", "oldString": "x", "newString": "y"})
            ),
            Some(json!({"path": "a.txt", "old": "x", "new": "y"}))
        );
        // A shell call's caption is left out, its cwd is the workdir, and a
        // timeout written as a string reads as a number.
        assert_eq!(
            with_field_aliases(
                &bash_like(),
                &json!({"command": "dir", "description": "List the folder", "cwd": "src", "timeout": "5000"})
            ),
            Some(json!({"command": "dir", "workdir": "src", "timeout": 5000}))
        );
        // Valid calls, a name the call also gives the tool's way, and any
        // other fault change nothing.
        assert_eq!(
            with_field_aliases(&edit_like(), &json!({"path": "a", "old": "x", "new": "y"})),
            None
        );
        assert_eq!(
            with_field_aliases(
                &edit_like(),
                &json!({"path": "a", "file_path": "b", "old": "x", "new": "y"})
            ),
            None
        );
        assert_eq!(
            with_field_aliases(&edit_like(), &json!({"file_path": "a", "old_string": "x"})),
            None
        );
        assert_eq!(
            with_field_aliases(
                &edit_like(),
                &json!({"file_path": "a", "old": "x", "new": "y", "force": true})
            ),
            None
        );
        // A tool that takes a description keeps it.
        let described = definition(json!({
            "type": "object",
            "properties": {"title": {"type": "string"}, "description": {"type": "string"}},
            "required": ["title"],
            "additionalProperties": false,
        }));
        assert_eq!(
            with_field_aliases(&described, &json!({"name": "x", "description": "y"})),
            None
        );
    }

    fn definition(schema: Value) -> ToolDefinition {
        ToolDefinition {
            name: "fixture".into(),
            description: String::new(),
            input_schema: schema,
        }
    }

    fn bash_like() -> ToolDefinition {
        definition(json!({
            "type": "object",
            "properties": {
                "command": {"type": "string"},
                "timeout": {"type": "integer", "minimum": 0},
                "run_in_background": {"type": "boolean"},
                "workdir": {"type": "string"},
                "ratio": {"type": "number"},
                "limit": {"type": ["integer", "null"]},
            },
            "required": ["command"],
            "additionalProperties": false,
        }))
    }

    #[test]
    fn scalars_a_model_wrote_as_strings_read_as_the_schemas_types() {
        let tool = bash_like();
        // Llama 3.1 8B on Ollama, running the tests.
        let written = json!({"command": "python3 test_calc.py", "timeout": "0", "run_in_background": "false", "workdir": "/p"});
        let coerced = coerced_arguments(&tool, &written).expect("coerced");
        assert_eq!(
            coerced,
            json!({"command": "python3 test_calc.py", "timeout": 0, "run_in_background": false, "workdir": "/p"})
        );
        validate_arguments(&tool, &coerced).unwrap();
        let one = |key: &str, text: &str| {
            coerced_arguments(&tool, &json!({"command": "x", key: text}))
                .map(|value| value[key].clone())
        };
        // JSON's own literals, the spaces around them aside.
        assert_eq!(
            one("timeout", "-3"),
            None,
            "below the minimum after reading"
        );
        assert_eq!(one("timeout", " 12 "), Some(json!(12)));
        assert_eq!(one("ratio", "1.5"), Some(json!(1.5)));
        assert_eq!(one("ratio", "-3"), Some(json!(-3)));
        assert_eq!(one("ratio", "1e5"), Some(json!(1e5)));
        assert_eq!(one("run_in_background", "true"), Some(json!(true)));
        // Python's spellings, as Llama 3.1 writes them.
        assert_eq!(one("run_in_background", "True"), Some(json!(true)));
        assert_eq!(one("run_in_background", "False"), Some(json!(false)));
        assert_eq!(one("limit", "null"), Some(json!(null)));
        assert_eq!(one("limit", "7"), Some(json!(7)));
        // An integer is written as one: no exponent, no fraction.
        for text in ["1e5", "1.5", "1.0"] {
            assert_eq!(one("timeout", text), None, "{text}");
        }
        // Not JSON literals.
        for text in [
            "01", "+1", ".5", "1.", "0x10", "NaN", "Infinity", "TRUE", "FALSE", "yes", "", "1 2",
            "\"1\"",
        ] {
            assert_eq!(one("timeout", text), None, "{text}");
            assert_eq!(one("ratio", text), None, "{text}");
            assert_eq!(one("run_in_background", text), None, "{text}");
        }
    }

    #[test]
    fn an_array_a_model_wrote_as_its_json_text_reads_as_the_array() {
        let tool = ToolDefinition {
            name: "multiedit".into(),
            description: String::new(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "edits": {"type": "array", "minItems": 1, "items": {
                        "type": "object",
                        "properties": {
                            "old": {"type": "string", "minLength": 1},
                            "new": {"type": "string"},
                            "replaceAll": {"type": "boolean"}
                        },
                        "required": ["old", "new"],
                        "additionalProperties": false
                    }}
                },
                "required": ["path", "edits"],
                "additionalProperties": false,
            }),
        };
        // Llama 3.1 8B on Ollama, renaming two names in one file.
        let written = json!({"path": "src/pricing.py", "edits": "[{\"new\": \"BULK_MINIMUM\", \"old\": \"BULK_THRESHOLD\", \"replaceAll\": false}, {\"new\": \"BULK_RATE\", \"old\": \"BULK_DISCOUNT\"}]"});
        let coerced = coerced_arguments(&tool, &written).expect("read as the array");
        assert_eq!(
            coerced["edits"],
            json!([
                {"new": "BULK_MINIMUM", "old": "BULK_THRESHOLD", "replaceAll": false},
                {"new": "BULK_RATE", "old": "BULK_DISCOUNT"}
            ])
        );
        // Scalars written as strings inside it read as theirs too.
        let nested = json!({"path": "a", "edits": " [{\"old\": \"x\", \"new\": \"y\", \"replaceAll\": \"true\"}] "});
        assert_eq!(
            coerced_arguments(&tool, &nested).expect("read twice")["edits"][0]["replaceAll"],
            json!(true)
        );
        // Qwen3 Coder 30B: the code it quotes keeps its line breaks and tabs.
        let lines = json!({"path": "a", "edits": "[{\"old\": \"if x:\n\treturn 1\", \"new\": \"if x:\n\treturn \\\"2\\\"\"}]"});
        assert_eq!(
            coerced_arguments(&tool, &lines).expect("read with its line breaks")["edits"],
            json!([{"old": "if x:\n\treturn 1", "new": "if x:\n\treturn \"2\""}])
        );
        // Text that is not the JSON of what the schema wants, or what then
        // fails the schema, changes nothing.
        for edits in [
            "old: x, new: y",
            "{\"old\": \"x\", \"new\": \"y\"}",
            "[]",
            "[{\"old\": \"\", \"new\": \"y\"}]",
            "[{\"old\": \"x\"",
        ] {
            assert_eq!(
                coerced_arguments(&tool, &json!({"path": "a", "edits": edits})),
                None,
                "{edits}"
            );
        }
    }

    fn read_like() -> ToolDefinition {
        ToolDefinition {
            name: "read".into(),
            description: String::new(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "offset": {"type": "integer", "minimum": 0},
                    "limit": {"type": "integer", "minimum": 1}
                },
                "required": ["path"],
                "additionalProperties": false,
            }),
        }
    }

    #[test]
    fn fields_a_tool_does_not_take_are_left_out_when_that_is_the_only_fault() {
        let tool = read_like();
        // gpt-oss 20B on Ollama, reading a file.
        let written = json!({"path": "README.md", "line_start": 1, "line_end": 400});
        let (kept, unknown) = without_unknown_fields(&tool, &written).expect("left out");
        assert_eq!(kept, json!({"path": "README.md"}));
        assert_eq!(unknown, ["line_start", "line_end"]);
        validate_arguments(&tool, &kept).unwrap();
        // With a number written as a string too, as Llama 3.1 writes them.
        let (kept, _) = without_unknown_fields(
            &tool,
            &json!({"path": "a", "limit": "50", "encoding": "utf-8"}),
        )
        .expect("left out and read");
        assert_eq!(kept, json!({"path": "a", "limit": 50}));
        // Valid arguments, another fault, or nothing unknown: no change.
        for unchanged in [
            json!({"path": "a"}),
            json!({"path": 42, "depth": 1}),
            json!({"line_start": 1}),
            json!({"path": "a", "limit": 0}),
        ] {
            assert_eq!(
                without_unknown_fields(&tool, &unchanged),
                None,
                "{unchanged}"
            );
        }
        // Arguments that could not be read never run, with or without fields.
        let unparsed = tool_call_arguments("{\"path\": \"a\",");
        assert_eq!(without_unknown_fields(&tool, &unparsed), None);
        // A schema that takes any field has nothing to leave out.
        let mut open = read_like();
        open.input_schema["additionalProperties"] = json!(true);
        assert_eq!(
            without_unknown_fields(&open, &json!({"path": 1, "x": 2})),
            None
        );
    }

    #[test]
    fn no_valid_call_and_no_other_fault_changes() {
        let tool = bash_like();
        // Valid: left alone, though `command` reads as a number.
        for valid in [
            json!({"command": "0"}),
            json!({"command": "true", "timeout": 5}),
            json!({"command": "x", "limit": null}),
        ] {
            assert_eq!(coerced_arguments(&tool, &valid), None, "{valid}");
        }
        // Another fault beside it: nothing changes, and both are reported.
        assert_eq!(coerced_arguments(&tool, &json!({"timeout": "0"})), None);
        assert_eq!(
            coerced_arguments(&tool, &json!({"command": "x", "timeout": "0", "extra": 1})),
            None
        );
        assert_eq!(
            coerced_arguments(&tool, &json!({"command": "x", "timeout": true})),
            None
        );
        // A string the schema wants a string for, refused for another reason.
        let short = definition(
            json!({"type": "object", "properties": {"code": {"type": "string", "minLength": 3}}}),
        );
        assert_eq!(coerced_arguments(&short, &json!({"code": "1"})), None);
        // An `enum` of numbers is no type to read a string as.
        let choice =
            definition(json!({"type": "object", "properties": {"level": {"enum": [1, 2, 3]}}}));
        assert_eq!(coerced_arguments(&choice, &json!({"level": "2"})), None);
    }

    #[test]
    fn nested_values_and_alternatives_are_read_where_the_schema_says() {
        let tool = definition(json!({
            "type": "object",
            "properties": {
                "edits": {"type": "array", "items": {
                    "type": "object",
                    "properties": {"line": {"type": "integer"}, "replace_all": {"type": "boolean"}},
                }},
                "pages": {"type": "array", "items": {"type": "integer"}},
                "only": {"anyOf": [{"type": "integer"}, {"type": "boolean"}]},
                "either": {"anyOf": [{"type": "integer"}, {"type": "number"}]},
                "one": {"oneOf": [{"type": "boolean"}, {"type": "array"}]},
                "text_too": {"anyOf": [{"type": "integer"}, {"type": "string"}]},
            },
        }));
        assert_eq!(
            coerced_arguments(
                &tool,
                &json!({"edits": [{"line": "3", "replace_all": "true"}], "pages": ["1", "2"]})
            ),
            Some(json!({"edits": [{"line": 3, "replace_all": true}], "pages": [1, 2]}))
        );
        // Exactly one alternative takes the value.
        assert_eq!(
            coerced_arguments(&tool, &json!({"only": "false"})),
            Some(json!({"only": false}))
        );
        assert_eq!(
            coerced_arguments(&tool, &json!({"one": "true"})),
            Some(json!({"one": true}))
        );
        // Two do: no telling which was meant.
        assert_eq!(coerced_arguments(&tool, &json!({"either": "5"})), None);
        // One takes the string itself: valid, unchanged.
        assert_eq!(coerced_arguments(&tool, &json!({"text_too": "5"})), None);
        // An alternative that leans on the rest of the schema is not read alone.
        let referred = definition(json!({
            "type": "object",
            "$defs": {"count": {"type": "integer"}},
            "properties": {"n": {"anyOf": [{"$ref": "#/$defs/count"}, {"type": "boolean"}]}},
        }));
        assert_eq!(coerced_arguments(&referred, &json!({"n": "4"})), None);
    }

    #[test]
    fn names_preserve_exact_matches_and_reject_ambiguity_and_guesses() {
        let resolve = |name| {
            resolve_name(
                name,
                ["read", "Read", "glob", "functions.Glob", "load_tool"].map(str::to_owned),
            )
        };
        assert_eq!(resolve("Read"), "Read");
        assert_eq!(resolve("READ"), "READ");
        assert_eq!(resolve("functions.READ"), "functions.READ");
        assert_eq!(resolve("GLOB"), "glob");
        assert_eq!(resolve("functions.glob"), "functions.Glob");
        assert_eq!(resolve("FUNCTIONS.LOAD_TOOL"), "load_tool");
        assert_eq!(resolve("glbo"), "glbo");
        assert_eq!(resolve("other.glob"), "other.glob");
        assert_eq!(resolve(" glob "), " glob ");
    }

    /// A workflow or Code Mode script the model wrote names an MCP tool the
    /// way providers saw it: that is the tool with its own name.
    #[test]
    fn a_tool_named_as_providers_saw_it_is_the_tool() {
        let resolve = |name| {
            resolve_name(
                name,
                [
                    "mcp__Docs Server__files.read",
                    "mcp__Docs_Server__plain",
                    "mcp__Docs Server__plain",
                    "read",
                ]
                .map(str::to_owned),
            )
        };
        assert_eq!(
            resolve("mcp__Docs_Server__files_read"),
            "mcp__Docs Server__files.read"
        );
        // The tool whose own name providers take keeps it.
        assert_eq!(
            resolve("mcp__Docs_Server__plain"),
            "mcp__Docs_Server__plain"
        );
        let names = crate::wire_names::WireNames::new([
            "mcp__Docs Server__plain",
            "mcp__Docs_Server__plain",
        ]);
        assert_eq!(
            resolve(names.wire("mcp__Docs Server__plain")),
            "mcp__Docs Server__plain"
        );
        assert_eq!(resolve("mcp__nothing__here"), "mcp__nothing__here");
    }

    /// `Google Drive` and `google_drive` both offer `search`: the name
    /// providers were sent for the first differs from the second's own name
    /// only in letter case, and still means the first.
    #[test]
    fn a_provider_name_wins_over_a_repair_of_letter_case() {
        let names = [
            "mcp__Google Drive__search".to_owned(),
            "mcp__google_drive__search".to_owned(),
        ];
        let resolve = |name| resolve_name(name, names.clone());
        for written in [
            "mcp__Google_Drive__search",
            "functions.mcp__Google_Drive__search",
        ] {
            assert_eq!(resolve(written), "mcp__Google Drive__search", "{written}");
        }
        assert_eq!(
            resolve("mcp__google_drive__search"),
            "mcp__google_drive__search"
        );
        // Letter case is still repaired for a tool's own name.
        assert_eq!(
            resolve("MCP__GOOGLE_DRIVE__SEARCH"),
            "mcp__google_drive__search"
        );
    }

    #[test]
    fn validation_reports_nested_paths_without_echoing_invalid_values() {
        let tool = definition(
            json!({"type":"object","properties":{"calls":{"type":"array","items":{
            "type":"object","properties":{"pattern":{"type":"string"},"count":{"type":"integer"}},
            "required":["pattern"],"additionalProperties":false
        }}},"required":["calls"]}),
        );
        let error = validate_arguments(
            &tool,
            &json!({"calls":[{}, {"pattern":42,"count":"secret-value"}]}),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("calls[0].pattern: required field is missing"),
            "{error}"
        );
        assert!(
            error.contains("calls[1].pattern: expected string"),
            "{error}"
        );
        assert!(
            error.contains("calls[1].count: expected integer"),
            "{error}"
        );
        assert!(!error.contains("secret-value"));
    }

    #[test]
    fn validation_handles_root_escaped_properties_refs_and_constraints() {
        let tool = definition(
            json!({"$defs":{"value":{"type":"string","enum":["yes","no"]}},
            "type":"object","properties":{"a/b":{"$ref":"#/$defs/value"},"0":{"type":"integer","minimum":1}},
            "required":["a/b"],"additionalProperties":false}),
        );
        assert!(validate_arguments(&tool, &json!({"a/b":"yes","0":1})).is_ok());
        let error = validate_arguments(&tool, &json!({"a/b":"maybe","0":0}))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("[\"a/b\"]: expected one of [\"yes\",\"no\"]"),
            "{error}"
        );
        assert!(error.contains("[\"0\"]: expected a value >= 1"), "{error}");
        assert!(
            validate_arguments(&tool, &Value::Null)
                .unwrap_err()
                .to_string()
                .contains("<root>: expected object")
        );
    }

    #[test]
    fn valid_optional_and_additional_arguments_are_not_rewritten() {
        let tool = definition(
            json!({"type":"object","properties":{"limit":{"type":["integer","null"]}},"additionalProperties":true}),
        );
        for arguments in [json!({}), json!({"limit":null,"extra":[1,2,3]})] {
            let original = arguments.clone();
            validate_arguments(&tool, &arguments).unwrap();
            assert_eq!(arguments, original);
        }
    }

    #[test]
    fn invalid_tool_schema_fails_without_guessing_arguments() {
        let error = validate_arguments(&definition(json!({"type":"not-a-json-type"})), &json!({}))
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid input schema"), "{error}");
        assert!(error.contains("tool definition must be fixed"), "{error}");
    }

    #[test]
    fn validation_feedback_is_bounded_without_limiting_arguments() {
        let tool = definition(json!({"type":"array","items":{"type":"integer"}}));
        let error = validate_arguments(&tool, &json!(vec!["wrong"; 20]))
            .unwrap_err()
            .to_string();
        assert_eq!(
            error
                .lines()
                .filter(|line| line.contains(": expected integer"))
                .count(),
            8
        );
        assert!(error.contains("Additional schema issues omitted"));
        validate_arguments(&tool, &json!(vec![1; 10000])).unwrap();
    }
}
