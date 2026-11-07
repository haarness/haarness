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

pub(crate) fn validate_arguments(tool: &ToolDefinition, arguments: &Value) -> CoreResult<()> {
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
/// `"run_in_background": "false"`), is that value, when every fault was such
/// a string and the arguments then pass. `None` otherwise: valid arguments,
/// any other fault, and a string the schema takes as a string are never
/// changed. In `anyOf` or `oneOf` the value is changed only when exactly one
/// alternative takes it. Nested objects and arrays are read the same way.
pub(crate) fn coerced_arguments(tool: &ToolDefinition, arguments: &Value) -> Option<Value> {
    let validator = jsonschema::validator_for(&tool.input_schema).ok()?;
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
                written_scalar(text).filter(|value| {
                    wanted.iter().any(|kind| match (kind, value) {
                        (JsonType::Boolean, Value::Bool(_))
                        | (JsonType::Null, Value::Null)
                        | (JsonType::Number, Value::Number(_)) => true,
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
    (changed && validator.is_valid(&coerced)).then_some(coerced)
}

/// The JSON literal a string is, ignoring the spaces around it, when it is
/// one that a model writes for a scalar: `true`, `false`, `null` or a number
/// in JSON's own form (`-3`, `1.5`, `1e5`; not `01`, `+1`, `.5`, `1.` or
/// `0x10`). An integer is one only within what a JSON integer holds.
fn written_scalar(text: &str) -> Option<Value> {
    let text = text.trim();
    match text {
        "true" => return Some(Value::Bool(true)),
        "false" => return Some(Value::Bool(false)),
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
        assert_eq!(one("limit", "null"), Some(json!(null)));
        assert_eq!(one("limit", "7"), Some(json!(7)));
        // An integer is written as one: no exponent, no fraction.
        for text in ["1e5", "1.5", "1.0"] {
            assert_eq!(one("timeout", text), None, "{text}");
        }
        // Not JSON literals.
        for text in [
            "01", "+1", ".5", "1.", "0x10", "NaN", "Infinity", "True", "FALSE", "yes", "", "1 2",
            "\"1\"",
        ] {
            assert_eq!(one("timeout", text), None, "{text}");
            assert_eq!(one("ratio", text), None, "{text}");
            assert_eq!(one("run_in_background", text), None, "{text}");
        }
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
